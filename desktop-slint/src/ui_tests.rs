//! Headless tests of the generated UI through Slint's testing backend. Each test runs on its own
//! thread, where it installs its own platform.

use crate::inspector::sample_view;
use crate::publisher::{install_models, SlintPublisher};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use mkv_anime_batch_slint_ui::{App, FontProbe, Mode, QueueFilter, Theme};
use mkv_desktop_core::actor::{Publisher, ScanView, ViewState};
use mkv_desktop_core::presentation::{self, Bucket, Tone};
use mkv_desktop_core::queue::Row;
use mkv_desktop_core::snapshot::ItemStatus;
use slint::{Color, ComponentHandle, Model};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// A window with its row models, as `main` builds it.
fn new_app() -> App {
    let app = App::new().unwrap();
    install_models(&app);
    app
}

/// Applies `state` to `app` as a hop to the event loop would, through a fresh publisher: its rows
/// replace the model's. A test of the in-place path keeps one publisher instead.
fn apply(app: &App, state: &ViewState) {
    SlintPublisher::new(app).show(state);
}

#[test]
fn the_window_shows_published_values_and_its_title_bar_buttons_answer() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
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
    let app = new_app();
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
    let app = new_app();
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
    let app = new_app();
    let mut state = ViewState::new(None);
    let mut running = row("Frieren - 02.mkv");
    running.status = ItemStatus::Processing;
    running.progress = Some(40.0);
    running.label = "40% · 3s".into();
    running.tone = Tone::Info;
    state.remux.rows = Arc::new(vec![row("Frieren - 01.mkv"), running]);
    state.remux.manifest_rev = 1;
    state.remux.empty = None;
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
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
    // The chip's label carries its count, as the webview chip's text does.
    only(&app, "Done 0").invoke_accessible_default_action();
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
    publisher.show(&state);
    calls.borrow_mut().clear();
    only(&app, "Include Frieren - 01.mkv in the batch").invoke_accessible_default_action();
    only(&app, "Select all").invoke_accessible_default_action();
    assert!(calls.borrow().is_empty());
    app.hide().unwrap();
}

#[test]
fn an_empty_queue_shows_the_empty_card_and_each_mode_its_own_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let mut state = ViewState::new(None);
    state.extract.rows = Arc::new(vec![row("Frieren - 01.mkv")]);
    state.extract.empty = None;
    state.extract.title = "Frieren".into();
    apply(&app, &state);
    // The window's real size, so the card has room to stretch into.
    app.window().set_size(slint::PhysicalSize::new(920, 640));
    app.show().unwrap();

    let card = only(&app, "No episodes yet");
    // The header keeps its own height over a card: the chips stay right under the title.
    let (title, chip) = (only(&app, "No folder open"), only(&app, "All 0"));
    let gap = chip.absolute_position().y - title.absolute_position().y;
    assert!(gap < 60.0, "the chips sit {gap}px below the title");
    assert!(card.absolute_position().y > chip.absolute_position().y);
    let opened = Rc::new(Cell::new(0));
    let counter = Rc::clone(&opened);
    app.on_open_folder(move || counter.set(counter.get() + 1));
    // The empty card's button and the Library's.
    let open_folder = || ElementHandle::find_by_accessible_label(&app, "Open folder…");
    assert_eq!(open_folder().count(), 2);
    open_folder().for_each(|button| button.invoke_accessible_default_action());
    assert_eq!(opened.get(), 2);
    // A batch locks both.
    app.set_busy(true);
    open_folder().for_each(|button| button.invoke_accessible_default_action());
    assert_eq!(opened.get(), 2);
    app.set_busy(false);
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
    // A loaded queue opens another folder from the Library.
    only(&app, "Open folder…").invoke_accessible_default_action();
    assert_eq!(opened.get(), 3);
    app.hide().unwrap();
}

/// A filter that matches no row says so; the rows stay in the model, hidden.
#[test]
fn a_filter_with_no_match_says_so() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let publisher = SlintPublisher::new(&app);
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Frieren - 01.mkv")]);
    state.remux.empty = None;
    publisher.show(&state);
    publisher.show_filter(Mode::Remux, QueueFilter::Failed);
    app.show().unwrap();
    assert_eq!(app.get_remux_head().filter, QueueFilter::Failed);
    only(&app, "No episodes match this filter.");
    assert_eq!(app.get_remux_rows().row_count(), 1);
    app.hide().unwrap();
}

/// One kept publisher across publications: a changed row and a reorder are written into the same
/// model the window binds.
#[test]
fn a_kept_model_takes_each_publication_in_place() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let publisher = SlintPublisher::new(&app);
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("a"), row("b"), row("c")]);
    state.remux.manifest_rev = 1;
    state.remux.empty = None;
    publisher.show(&state);
    let model = app.get_remux_rows();
    let labels = || {
        (0..model.row_count())
            .map(|i| model.row_data(i).unwrap())
            .map(|row| format!("{} {}", row.id, row.label))
            .collect::<Vec<_>>()
    };
    assert_eq!(labels(), ["a Queued", "b Queued", "c Queued"]);

    let mut rows = (*state.remux.rows).clone();
    rows[1].label = "40% · 3s".into();
    state.seq = 1;
    state.remux.rows = Arc::new(rows.clone());
    publisher.show(&state);
    assert_eq!(labels(), ["a Queued", "b 40% · 3s", "c Queued"]);
    // The window still binds the model the rows were written into (which rows a patch writes is
    // `queue::diff_rows`'s test).
    assert!(app.get_remux_rows() == model);

    rows.swap(0, 2);
    state.seq = 2;
    state.remux.rows = Arc::new(rows);
    publisher.show(&state);
    assert_eq!(labels(), ["c Queued", "b 40% · 3s", "a Queued"]);
    assert!(app.get_remux_rows() == model);
}

/// Each mode keeps its own scroll offset, as the webview's two lists do, to the pixel with rows
/// of different heights (a ListView made again would place them by an estimated height); a card
/// (a scan of another folder) puts its list back at the top.
#[test]
fn each_mode_keeps_its_scroll_and_a_new_folder_starts_at_the_top() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let publisher = SlintPublisher::new(&app);
    // Two releases, so the first row and the 21st carry a group head.
    let rows = |prefix: &str| {
        Arc::new(
            (0..40)
                .map(|i| Row {
                    release: if i < 20 { "." } else { "S2" }.into(),
                    ..row(&format!("{prefix}{i:02}"))
                })
                .collect::<Vec<_>>(),
        )
    };
    let mut state = ViewState::new(None);
    for (view, prefix) in [(&mut state.remux, "a"), (&mut state.extract, "b")] {
        view.rows = rows(prefix);
        view.manifest_rev = 1;
        view.empty = None;
        view.directory = Some(format!("/{prefix}"));
    }
    publisher.show(&state);
    app.show().unwrap();
    // Where a row's checkbox is drawn.
    let y = |id: &str| {
        only(&app, &format!("Include {id} in the batch"))
            .absolute_position()
            .y
    };
    let top = y("a05");

    app.set_remux_scroll(-600.0);
    let scrolled = y("a14");
    only(&app, "Extract").invoke_accessible_default_action();
    assert_eq!(y("b05"), top, "Extract has its own offset");
    app.set_extract_scroll(-52.0);
    only(&app, "Remux").invoke_accessible_default_action();
    assert_eq!(y("a14"), scrolled, "Remux keeps its offset");

    // Another folder: the scanning card, then its rows, from the top.
    state.seq = 1;
    state.remux.scan = ScanView::Scanning {
        directory: "/c".into(),
    };
    publisher.show(&state);
    only(&app, "Scanning folder");
    state.seq = 2;
    state.remux.scan = ScanView::Idle;
    state.remux.rows = rows("c");
    state.remux.manifest_rev = 2;
    state.remux.directory = Some("/c".into());
    publisher.show(&state);
    assert_eq!(y("c05"), top);
    // The other mode's offset is its own.
    assert_eq!(app.get_extract_scroll(), -52.0);

    // The card and the next folder's rows taken in one hop still start the rows at the top.
    app.set_remux_scroll(-600.0);
    let mut scanning = state.clone();
    scanning.seq = 3;
    scanning.remux.scan = ScanView::Scanning {
        directory: "/d".into(),
    };
    state.seq = 4;
    state.remux.rows = rows("d");
    state.remux.manifest_rev = 3;
    state.remux.directory = Some("/d".into());
    publisher.show_coalesced(&[&scanning, &state]);
    assert_eq!(y("d05"), top);
    assert_eq!(app.get_extract_scroll(), -52.0);
    app.hide().unwrap();
}

/// Three releases of 20, 20 and 4 rows, in one folder, with the Library the core publishes for
/// them.
fn three_releases() -> ViewState {
    let rows: Vec<Row> = (0..44)
        .map(|i| Row {
            release: [".", "S2", "S3"][i / 20].into(),
            ..row(&format!("{}{:02}", ["a", "b", "c"][i / 20], i % 20))
        })
        .collect();
    let mut state = ViewState::new(None);
    state.remux.library = presentation::library(Some("/media/Show"), &rows);
    state.remux.rows = Arc::new(rows);
    state.remux.manifest_rev = 1;
    state.remux.empty = None;
    state.remux.directory = Some("/media/Show".into());
    state
}

/// Lays the window out and runs the change handlers its layout set off, as a frame does.
fn frame(app: &App) {
    for _ in 0..2 {
        // A search visits the whole tree, which lays the lists out.
        ElementHandle::find_by_accessible_label(app, "").count();
        slint::platform::update_timers_and_animations();
    }
}

/// The Library lists the folder and its releases; a release's entry shows every row again and
/// puts the release's first row at the top of the list, to the pixel, with rows of two heights,
/// even from a filter that hid every row (which leaves the list no row height to measure by).
#[test]
fn the_library_reveals_a_release_at_the_top_of_its_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let publisher = SlintPublisher::new(&app);
    publisher.show(&three_releases());
    app.show().unwrap();
    let y = |id: &str| {
        only(&app, &format!("Include {id} in the batch"))
            .absolute_position()
            .y
    };
    let top = y("a00");

    let clicks = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&clicks);
    app.on_reveal_release(move |release, root| log.borrow_mut().push((release.to_string(), root)));
    only(&app, "Show 44").invoke_accessible_default_action();
    only(&app, "S2 20").invoke_accessible_default_action();
    assert_eq!(
        *clicks.borrow(),
        [(String::new(), true), ("S2".to_string(), false)]
    );

    // From a filter that hid every row, laid out: the reveal shows every row again.
    publisher.show_filter(Mode::Remux, QueueFilter::Done);
    frame(&app);
    publisher.show_reveal(Mode::Remux, "S2");
    frame(&app);
    assert_eq!(app.get_remux_head().filter, QueueFilter::All);
    assert_eq!(y("b00"), top, "S2's head is the list's first line");
    // Back up from below.
    publisher.show_reveal(Mode::Remux, "S2");
    assert_eq!(y("b00"), top);
    // The last release cannot reach the top: the list ends at its last row, as scrolling into
    // view stops at the end.
    publisher.show_reveal(Mode::Remux, "S3");
    let last = only(&app, "Include c03 in the batch");
    assert!(y("c00") > top && last.absolute_position().y < 640.0);

    // The top (the folder's entry, or a card), from a filter that hid every row.
    publisher.show_filter(Mode::Remux, QueueFilter::Done);
    frame(&app);
    publisher.show_filter(Mode::Remux, QueueFilter::All);
    app.invoke_reveal(Mode::Remux, 0);
    frame(&app);
    assert_eq!(y("a00"), top, "the folder's first row heads the list");

    // A filter clicked after a release's entry wins: the list does not scroll to the release.
    publisher.reveal(Mode::Remux, "S2".into());
    publisher.show_filter(Mode::Remux, QueueFilter::All);
    frame(&app);
    assert_eq!(y("a00"), top);
    // So does one clicked while the reveal waits for the rows' layout.
    publisher.show_filter(Mode::Remux, QueueFilter::Done);
    frame(&app);
    publisher.show_reveal(Mode::Remux, "S2");
    only(&app, "Done 0").invoke_accessible_default_action();
    frame(&app);
    assert_eq!(y("a00"), top);
    app.hide().unwrap();
}

/// A published remux Inspector at the narrowest window: every field shows, the subtitle controls
/// carry their labels and fit the pane; an assignment brings Reset and its options.
#[test]
fn the_inspector_shows_every_field_and_fits_the_narrowest_window() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    let publisher = SlintPublisher::new(&app);
    let mut state = ViewState::new(None);
    app.show().unwrap();
    publisher.show(&state);
    only(
        &app,
        "Open a folder, then pick an episode to see what the batch does with it.",
    );

    state.seq = 1;
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(sample_view());
    publisher.show(&state);
    for label in [
        "03",
        "ep1.mkv",
        "media › Season 1",
        "40% · 3s",
        "HEVC 1080p · copied",
        "FLAC 2.0 → Opus 192k",
        "Output folder next to the input",
        "2 Same folder, Season 1 › ep1.ass, Auto",
        "FONTS · 2",
        "2 attached fonts · folder: auto",
        "Assign subtitle",
    ] {
        only(&app, label);
    }
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Reset").count(),
        0
    );
    assert_eq!(app.get_sub_options().row_count(), 3);
    // The list's tools wrap in the list left beside the Inspector (.list-tools): every chip and
    // link shows whole, inside the list, none over another.
    let list_end = 760.0 - 248.0;
    let tools: Vec<_> = [
        "All 0",
        "To do 0",
        "Done 0",
        "Failed 0",
        "Select all",
        "None",
    ]
    .map(|label| only(&app, label))
    .into_iter()
    .map(|tool| (tool.absolute_position(), tool.size()))
    .collect();
    for (at, size) in &tools {
        assert!(
            at.x >= 196.0 && at.x + size.width <= list_end,
            "{at:?} {size:?}"
        );
    }
    for (i, (a, a_size)) in tools.iter().enumerate() {
        for (b, b_size) in &tools[i + 1..] {
            let apart = a.x + a_size.width <= b.x
                || b.x + b_size.width <= a.x
                || a.y + a_size.height <= b.y
                || b.y + b_size.height <= a.y;
            assert!(apart, "{a:?} overlaps {b:?}");
        }
    }
    assert!(
        tools[5].0.y > tools[0].0.y,
        "the tools wrap onto a second line"
    );

    let mut assigned = sample_view();
    assigned.assignment = Some(mkv_core::request::TrackAssignment {
        target_video: "Season 1/ep1.mkv".into(),
        asset_path: "Season 1/ep1.ass".into(),
        track_type: Default::default(),
        language: Some("may".into()),
        title: Some("Signs".into()),
        offset_ms: Some(120),
    });
    assigned.language_choice = "custom";
    assigned.custom_language = "may".into();
    state.seq = 2;
    state.remux.inspector = Some(assigned);
    publisher.show(&state);
    assert_eq!(
        (app.get_sub_choice(), app.get_sub_language()),
        (1, 3),
        "the published fields are written back"
    );
    assert_eq!(
        (
            app.get_sub_custom(),
            app.get_sub_title(),
            app.get_sub_offset()
        ),
        ("may".into(), "Signs".into(), "120".into())
    );
    // The pane's sections scroll; the subtitle options are at their end.
    scroll(&app, 640.0, false);
    for label in [
        "Reset",
        "Language",
        "Language tag",
        "Track title",
        "Offset (ms)",
    ] {
        let control = only(&app, label);
        let right = control.absolute_position().x + control.size().width;
        assert!(
            right <= 760.0,
            "{label} ends at {right}, past the window's 760"
        );
    }

    scroll(&app, 640.0, true);

    // The extract Inspector has no subtitle section.
    state.seq = 3;
    state.extract.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.extract.empty = None;
    state.extract.inspector = Some(sample_view());
    publisher.show(&state);
    app.set_mode(Mode::Extract);
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Assign subtitle").count(),
        0
    );
    only(&app, "Same folder as the input");
    app.hide().unwrap();
}

/// Scrolls the pane at `x` to its end with the mouse wheel, or back to its top for `up`.
fn scroll(app: &App, x: f32, up: bool) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(x, 300.0),
            delta_x: 0.0,
            delta_y: if up { 5000.0 } else { -5000.0 },
        });
}

/// A folder of 30 releases overflows the sidebar at the narrowest window: it scrolls, so Open
/// folder stays reachable, as `.side`'s overflow keeps it.
#[test]
fn the_sidebar_scrolls_to_open_folder_past_many_releases() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    let rows: Vec<Row> = (0..30)
        .map(|i| Row {
            release: format!("R{i:02}"),
            ..row(&format!("e{i:02}"))
        })
        .collect();
    let mut state = ViewState::new(None);
    state.remux.library = presentation::library(Some("/media/Show"), &rows);
    state.remux.rows = Arc::new(rows);
    state.remux.empty = None;
    apply(&app, &state);
    app.show().unwrap();
    // Clipped out of the sidebar: the search finds only what shows.
    let open_folder = || ElementHandle::find_by_accessible_label(&app, "Open folder…").count();
    assert_eq!(open_folder(), 0, "30 releases overflow the window");
    scroll(&app, 90.0, false);
    let open = only(&app, "Open folder…");
    let end = open.absolute_position().y + open.size().height;
    assert!(end <= 520.0, "Open folder ends at {end}, past the window");
    app.hide().unwrap();
}

/// A title typed and not committed is committed before a click on a control that takes no focus
/// (a row, Open folder, a filter chip, a Library entry, a mode tab, Reset), as the webview's blur
/// commits it before the click lands; an answered Intent's write-back keeps what is typed
/// meanwhile.
#[test]
fn a_typed_subtitle_field_is_committed_before_a_click_elsewhere() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut assigned = sample_view();
    assigned.assignment = Some(mkv_core::request::TrackAssignment {
        target_video: "Season 1/ep1.mkv".into(),
        asset_path: "Season 1/ep1.ass".into(),
        track_type: Default::default(),
        language: Some("eng".into()),
        title: None,
        offset_ms: None,
    });
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv"), row("Show - 02.mkv")]);
    state.remux.empty = None;
    state.remux.library = presentation::library(Some("/media/Show"), &state.remux.rows);
    state.remux.inspector = Some(assigned);
    apply(&app, &state);
    app.show().unwrap();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&calls);
    app.on_subtitle_options_changed(move || {
        let title = log.borrow().len();
        log.borrow_mut().push(format!("options {title}"))
    });
    let log = Rc::clone(&calls);
    app.on_inspect(move |id| log.borrow_mut().push(id.to_string()));
    let log = Rc::clone(&calls);
    app.on_open_folder(move || log.borrow_mut().push("open".into()));
    let log = Rc::clone(&calls);
    app.on_set_filter(move |filter| log.borrow_mut().push(format!("{filter:?}")));
    let log = Rc::clone(&calls);
    app.on_reveal_release(move |_, _| log.borrow_mut().push("library".into()));

    only(&app, "Show - 02.mkv").invoke_accessible_default_action();
    only(&app, "Track title").set_accessible_value("Signs");
    // An answer's write-back on the same row keeps the typed title for its commit.
    crate::inspector::restore_form(&app);
    assert_eq!(app.get_sub_title(), "Signs");
    only(&app, "Show - 01.mkv").invoke_accessible_default_action();
    for click in ["Open folder…", "Done 0", "Show 2", "Extract"] {
        only(&app, "Track title").set_accessible_value(click);
        only(&app, click).invoke_accessible_default_action();
        if click == "Extract" {
            only(&app, "Remux").invoke_accessible_default_action();
        }
    }
    assert_eq!(
        *calls.borrow(),
        [
            "Show - 02.mkv",
            "options 1",
            "Show - 01.mkv",
            "options 3",
            "open",
            "options 5",
            "Done",
            "options 7",
            "library",
            "options 9",
        ],
        "committed once, before each click"
    );
    // Reset commits the typed title, then clears; the answer's write-back drops the title.
    let log = Rc::clone(&calls);
    app.on_reset_subtitle(move || log.borrow_mut().push("reset".into()));
    only(&app, "Track title").set_accessible_value("Signs");
    only(&app, "Reset").invoke_accessible_default_action();
    assert_eq!(calls.borrow()[10..], ["options 10", "reset"]);
    crate::inspector::restore_form(&app);
    assert_eq!(
        (app.get_sub_title().as_str(), app.get_sub_dirty()),
        ("", false)
    );
    // Nothing typed since: nothing to commit.
    only(&app, "Show - 02.mkv").invoke_accessible_default_action();
    assert_eq!(calls.borrow().last().unwrap(), "Show - 02.mkv");
    app.hide().unwrap();
}

/// Picking "Custom tag…" puts the cursor in the tag field, also when it already shows.
#[test]
fn picking_the_custom_tag_focuses_the_tag_field() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut assigned = sample_view();
    assigned.assignment = Some(mkv_core::request::TrackAssignment {
        target_video: "Season 1/ep1.mkv".into(),
        asset_path: "Season 1/ep1.ass".into(),
        track_type: Default::default(),
        language: Some("may".into()),
        title: None,
        offset_ms: None,
    });
    assigned.language_choice = "custom";
    assigned.custom_language = "ma".into();
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(assigned);
    apply(&app, &state);
    app.show().unwrap();
    frame(&app);

    // The language select's "Custom tag…" picked again, from its popup.
    only(&app, "Language").invoke_accessible_expand_action();
    frame(&app);
    only(&app, "Custom tag…").mock_single_click(slint::platform::PointerEventButton::Left);
    frame(&app);
    // Typing goes to the field with the focus.
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyPressed { text: "y".into() });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::KeyReleased { text: "y".into() });
    assert!(
        app.get_sub_custom().contains('y'),
        "typed into the tag field"
    );
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
    let app = new_app();
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
            ("info-soft", theme.get_info_soft()),
            ("ok-soft", theme.get_ok_soft()),
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
            // The filter chip that is on.
            ("bg", "fg", theme.get_bg(), theme.get_fg()),
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
