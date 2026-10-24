//! Headless tests of the generated UI through Slint's testing backend. Each test runs on its own
//! thread, where it installs its own platform.

use crate::publisher::SlintPublisher;
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use mkv_anime_batch_slint_ui::{App, FontProbe, Mode, QueueFilter, Theme};
use mkv_desktop_core::actor::{Publisher, ViewState};
use mkv_desktop_core::presentation::{Bucket, Tone};
use mkv_desktop_core::queue::Row;
use mkv_desktop_core::snapshot::ItemStatus;
use slint::{Color, ComponentHandle, Model};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// Applies `state` to `app` as a hop to the event loop would.
fn apply(app: &App, state: &ViewState) {
    SlintPublisher::new(app).show(state);
}

#[test]
fn the_window_shows_published_values_and_its_title_bar_buttons_answer() {
    i_slint_backend_testing::init_no_event_loop();
    let app = App::new().unwrap();
    let mut state = ViewState::new(None);
    state.remux.title = "Frieren".into();
    state.remux.count_text = "12 episodes".into();
    state.busy = true;
    apply(&app, &state);
    app.show().unwrap();

    for text in ["Frieren", "12 episodes"] {
        assert_eq!(
            ElementHandle::find_by_accessible_label(&app, text).count(),
            1,
            "{text:?} is shown"
        );
    }
    assert!(app.get_busy());

    let closes = Rc::new(Cell::new(0));
    let minimizes = Rc::new(Cell::new(0));
    let counter = Rc::clone(&closes);
    app.on_close_window(move || counter.set(counter.get() + 1));
    let counter = Rc::clone(&minimizes);
    app.on_minimize_window(move || counter.set(counter.get() + 1));
    let press = |label: &str| {
        let mut buttons: Vec<_> = ElementHandle::find_by_accessible_label(&app, label).collect();
        assert_eq!(buttons.len(), 1, "one {label:?} button");
        buttons.pop().unwrap().invoke_accessible_default_action();
    };

    press("Close window");
    press("Minimize window");
    press("Minimize window");
    assert_eq!((closes.get(), minimizes.get()), (1, 2));
    press("Maximize window");
    assert!(app.window().is_maximized());
    press("Maximize window");
    assert!(!app.window().is_maximized());
    app.hide().unwrap();
}

/// A long teardown error wraps, and the notice grows to hold it: the list header starts below it.
#[test]
fn a_long_close_notice_wraps_and_pushes_the_list_down() {
    i_slint_backend_testing::init_no_event_loop();
    let app = App::new().unwrap();
    let mut state = ViewState::new(None);
    state.remux.title = "Frieren".into();
    apply(&app, &state);
    let error = "process cleanup failed: ".repeat(30);
    app.set_notice_title("Could not close safely".into());
    app.set_notice_text(error.as_str().into());
    app.show().unwrap();

    let only = |label: &str| {
        let mut found: Vec<_> = ElementHandle::find_by_accessible_label(&app, label).collect();
        assert_eq!(found.len(), 1, "one {label:?}");
        found.pop().unwrap()
    };
    let (title, body, header) = (
        only("Could not close safely"),
        only(&error),
        only("Frieren"),
    );
    assert!(
        body.size().height >= 3.0 * title.size().height,
        "the error wraps onto several lines"
    );
    assert!(
        header.absolute_position().y >= body.absolute_position().y + body.size().height,
        "the list header starts below the notice's last line"
    );
    app.hide().unwrap();
}

/// Publications from another thread hop to the event loop, where the newest one is bound. The only
/// test with a threaded platform: Slint keeps one event loop proxy per process.
#[test]
fn a_publication_from_another_thread_reaches_the_window() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: true,
        ..Default::default()
    })))
    .unwrap();
    let app = App::new().unwrap();
    let publisher = SlintPublisher::new(&app);
    let worker = std::thread::spawn(move || {
        for (seq, title) in [(1, "Older"), (2, "Frieren")] {
            let mut state = ViewState::new(None);
            state.seq = seq;
            state.remux.title = title.into();
            state.busy = true;
            publisher.state(&state);
        }
        // Queued behind the hop, so the loop applies it first.
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop().unwrap();
    worker.join().unwrap();
    assert_eq!(app.get_remux_head().title, "Frieren");
    assert!(app.get_busy());
}

/// A selected, queued row named `id`.
fn row(id: &str) -> Row {
    Row {
        id: id.into(),
        name: id.into(),
        release: ".".into(),
        in_manifest: true,
        selected: true,
        status: ItemStatus::Queued,
        progress: None,
        participant: false,
        label: "Queued".into(),
        tone: Tone::Mute,
        title: String::new(),
        meta: String::new(),
        meta_error: false,
        bucket: Some(Bucket::Todo),
    }
}

/// The one element labelled `label`.
fn only(app: &App, label: &str) -> ElementHandle {
    let mut found: Vec<_> = ElementHandle::find_by_accessible_label(app, label).collect();
    assert_eq!(found.len(), 1, "one {label:?}");
    found.pop().unwrap()
}

#[test]
fn rows_render_with_their_accessible_labels_and_their_controls_answer() {
    i_slint_backend_testing::init_no_event_loop();
    let app = App::new().unwrap();
    let mut state = ViewState::new(None);
    let mut running = row("Frieren - 02.mkv");
    running.status = ItemStatus::Processing;
    running.progress = Some(40.0);
    running.label = "40% · 3s".into();
    running.tone = Tone::Info;
    state.remux.rows = Arc::new(vec![row("Frieren - 01.mkv"), running]);
    state.remux.manifest_rev = 1;
    state.remux.empty = None;
    apply(&app, &state);
    app.show().unwrap();

    for label in [
        "Include Frieren - 01.mkv in the batch",
        "Include Frieren - 02.mkv in the batch",
        "Frieren - 01.mkv",
        "40% · 3s",
    ] {
        only(&app, label);
    }
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Move up").count(),
        2
    );
    assert_eq!(app.get_remux_rows().row_count(), 2);

    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = Rc::clone(&calls);
    app.on_set_selected(move |id, selected| {
        log.borrow_mut().push(format!("select {id} {selected}"))
    });
    let log = Rc::clone(&calls);
    app.on_set_all_selected(move |selected| log.borrow_mut().push(format!("all {selected}")));
    let log = Rc::clone(&calls);
    app.on_reorder(move |id, up| log.borrow_mut().push(format!("reorder {id} {up}")));
    let log = Rc::clone(&calls);
    app.on_inspect(move |id| log.borrow_mut().push(format!("inspect {id}")));
    let log = Rc::clone(&calls);
    app.on_set_filter(move |filter| log.borrow_mut().push(format!("filter {filter:?}")));

    only(&app, "Include Frieren - 01.mkv in the batch").invoke_accessible_default_action();
    only(&app, "Select all").invoke_accessible_default_action();
    only(&app, "None").invoke_accessible_default_action();
    let downs: Vec<_> = ElementHandle::find_by_accessible_label(&app, "Move down").collect();
    downs[0].invoke_accessible_default_action();
    only(&app, "Frieren - 02.mkv").invoke_accessible_default_action();
    only(&app, "Done").invoke_accessible_default_action();
    assert_eq!(
        *calls.borrow(),
        [
            "select Frieren - 01.mkv false",
            "all true",
            "all false",
            "reorder Frieren - 01.mkv false",
            "inspect Frieren - 02.mkv",
            "filter Done",
        ]
    );
    // Marked at once, before the core answers.
    assert_eq!(app.get_pending_inspect(), "Frieren - 02.mkv");

    // A locked selection refuses the checkbox and the bulk buttons.
    state.seq = 1;
    state.remux.controls.selection_locked = true;
    apply(&app, &state);
    calls.borrow_mut().clear();
    only(&app, "Include Frieren - 01.mkv in the batch").invoke_accessible_default_action();
    only(&app, "Select all").invoke_accessible_default_action();
    assert!(calls.borrow().is_empty());
    app.hide().unwrap();
}

#[test]
fn an_empty_queue_shows_the_empty_card_and_each_mode_its_own_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = App::new().unwrap();
    let mut state = ViewState::new(None);
    state.extract.rows = Arc::new(vec![row("Frieren - 01.mkv")]);
    state.extract.empty = None;
    state.extract.title = "Frieren".into();
    apply(&app, &state);
    app.show().unwrap();

    only(&app, "No episodes yet");
    let opened = Rc::new(Cell::new(0));
    let counter = Rc::clone(&opened);
    app.on_open_folder(move || counter.set(counter.get() + 1));
    only(&app, "Open folder…").invoke_accessible_default_action();
    assert_eq!(opened.get(), 1);
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Frieren - 01.mkv").count(),
        0
    );

    only(&app, "Extract").invoke_accessible_default_action();
    assert_eq!(app.get_mode(), Mode::Extract);
    only(&app, "Frieren");
    only(&app, "Frieren - 01.mkv");
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "No episodes yet").count(),
        0
    );
    app.hide().unwrap();
}

/// A filter that matches no row says so; the rows stay in the model, hidden.
#[test]
fn a_filter_with_no_match_says_so() {
    i_slint_backend_testing::init_no_event_loop();
    let app = App::new().unwrap();
    let publisher = SlintPublisher::new(&app);
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Frieren - 01.mkv")]);
    state.remux.empty = None;
    publisher.show(&state);
    publisher.set_filter(Mode::Remux, QueueFilter::Failed);
    // set_filter schedules a hop; apply it as the event loop would.
    state.seq = 1;
    publisher.show(&state);
    app.show().unwrap();
    assert_eq!(app.get_remux_head().filter, QueueFilter::Failed);
    only(&app, "No episodes match this filter.");
    assert_eq!(app.get_remux_rows().row_count(), 1);
    app.hide().unwrap();
}

/// WCAG relative luminance of an sRGB colour.
fn luminance(color: Color) -> f64 {
    let channel = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.red()) + 0.7152 * channel(color.green()) + 0.0722 * channel(color.blue())
}

fn contrast(a: Color, b: Color) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `top` composited over the opaque `below`.
fn over(top: Color, below: Color) -> Color {
    let alpha = f64::from(top.alpha()) / 255.0;
    // Rounded and within 0..=255, so the cast loses nothing.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mix = |t: u8, b: u8| (f64::from(t) * alpha + f64::from(b) * (1.0 - alpha)).round() as u8;
    Color::from_rgb_u8(
        mix(top.red(), below.red()),
        mix(top.green(), below.green()),
        mix(top.blue(), below.blue()),
    )
}

/// Every text token over every surface, bare or under each tint drawn on it (field, sel, hover and
/// the soft tones), each surface over each colour of the wallpaper (its base and the three
/// gradients' peaks), and each button's text over its fill: 4.5:1 (WCAG AA body text) in both themes. Reads the tokens from the generated `Theme`, the
/// one place they are defined.
#[test]
fn every_text_token_meets_wcag_aa_on_every_surface_in_both_themes() {
    i_slint_backend_testing::init_no_event_loop();
    let app = App::new().unwrap();
    let theme = app.global::<Theme>();
    let mut failures = Vec::new();
    for dark in [false, true] {
        theme.invoke_set_dark(dark);
        assert_eq!(theme.get_dark(), dark);
        let name = if dark { "dark" } else { "light" };
        let walls = [
            ("wall-base", theme.get_wall_base()),
            ("wall-a", theme.get_wall_a()),
            ("wall-b", theme.get_wall_b()),
            ("wall-c", theme.get_wall_c()),
        ];
        let texts = [
            ("fg", theme.get_fg()),
            ("mute", theme.get_mute()),
            ("path", theme.get_path()),
            ("accent", theme.get_accent()),
            ("ok", theme.get_ok()),
            ("warn", theme.get_warn()),
            ("err", theme.get_err()),
            ("info", theme.get_info()),
            ("done", theme.get_done()),
        ];
        let surfaces = [
            ("bg", theme.get_bg()),
            ("side", theme.get_side()),
            ("panel", theme.get_panel()),
            ("content", theme.get_content()),
            ("head", theme.get_head()),
            ("float", theme.get_float()),
        ];
        let tints = [
            ("field", theme.get_field()),
            ("sel", theme.get_sel()),
            ("hover", theme.get_hover()),
            ("accent-soft", theme.get_accent_soft()),
            ("err-soft", theme.get_err_soft()),
        ];
        let mut pairs = Vec::new();
        for (surface, fill) in surfaces {
            for (wall, below) in walls {
                let pane = over(fill, below);
                let mut backgrounds = vec![(format!("{surface} over {wall}"), pane)];
                for (tint, tinted) in tints {
                    backgrounds.push((
                        format!("{tint} on {surface} over {wall}"),
                        over(tinted, pane),
                    ));
                }
                for (on, background) in backgrounds {
                    for (text, color) in texts {
                        pairs.push((text, on.clone(), color, background));
                    }
                }
            }
        }
        for (text, fill, color, background) in [
            (
                "accent-fg",
                "accent",
                theme.get_accent_fg(),
                theme.get_accent(),
            ),
            ("run-fg", "run", theme.get_run_fg(), theme.get_run()),
            ("err-fg", "err", theme.get_err_fg(), theme.get_err()),
        ] {
            pairs.push((text, fill.to_string(), color, background));
        }
        for (text, surface, color, background) in pairs {
            let ratio = contrast(color, background);
            if ratio < 4.5 {
                failures.push(format!("{name}: {text} on {surface}: {ratio:.2}"));
            }
        }
    }
    assert!(failures.is_empty(), "below 4.5:1:\n{}", failures.join("\n"));
}

/// Renders the probe line in `family` at `weight` and sums its ink: black text on white.
fn ink(family: &str, weight: i32) -> u64 {
    ink_of(family, weight, None)
}

/// Renders `text`, or the probe's own line, as [`ink`] does.
fn ink_of(family: &str, weight: i32, text: Option<&str>) -> u64 {
    let probe = FontProbe::new().unwrap();
    probe.set_family(family.into());
    probe.set_weight(weight);
    if let Some(text) = text {
        probe.set_text(text.into());
    }
    probe.show().unwrap();
    let shot = probe.window().take_snapshot().unwrap();
    probe.hide().unwrap();
    shot.as_slice()
        .iter()
        .map(|pixel| u64::from(255 - pixel.r))
        .sum()
}

/// The embedded variable fonts resolve, and their weights come from the `wght` axis: a heavy line
/// carries far more ink than a light one, and none renders like the fallback font.
#[test]
fn fonts_resolve_their_weights() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let fallback = ink("No Such Family", 400);
    for family in ["Onest", "Unbounded", "JetBrains Mono"] {
        let (light, regular, heavy) = (ink(family, 200), ink(family, 400), ink(family, 800));
        assert_ne!(regular, fallback, "{family} is not the fallback font");
        assert!(
            light < regular && regular < heavy && heavy > light * 2,
            "{family}: ink at 200/400/800 is {light}/{regular}/{heavy}"
        );
    }
}

/// Chinese and Japanese file names render through the system's font fallback (the embedded fonts
/// are Latin-only): each draws differently from as many missing glyphs, which all look alike.
#[test]
fn cjk_names_render_through_the_system_font_fallback() {
    // A Linux machine without a CJK font (a bare CI runner) has nothing to fall back to.
    let no_cjk_font = cfg!(target_os = "linux")
        && std::process::Command::new("fc-list")
            .args([":lang=ja", "family"])
            .output()
            .map_or(true, |out| out.stdout.is_empty());
    if no_cjk_font {
        eprintln!("skipped: no CJK system font (fc-list :lang=ja is empty)");
        return;
    }
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    for name in ["葬送のフリーレン", "药屋少女的呢喃"] {
        // Private Use Area code points: no font has them.
        let missing: String = name.chars().map(|_| '\u{E000}').collect();
        let (shown, tofu) = (
            ink_of("Onest", 400, Some(name)),
            ink_of("Onest", 400, Some(&missing)),
        );
        assert!(
            shown > 0 && shown != tofu,
            "{name}: ink {shown}, missing glyphs {tofu}"
        );
    }
}
