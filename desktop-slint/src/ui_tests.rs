//! Headless tests of the generated UI through Slint's testing backend. Each test runs on its own
//! thread, where it installs its own platform.

use crate::inspector::sample_view;
use crate::publisher::{install_models, SlintPublisher};
use i_slint_backend_testing::{ElementHandle, TestingBackend, TestingBackendOptions};
use mkv_anime_batch_slint_ui::{App, Focus, FontProbe, Mode, QueueFilter, Sheet, Theme};
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

/// The middle of the gap between the close and the minimize lights.
fn traffic_light_gap(app: &App) -> slint::LogicalPosition {
    let light = only(app, "Close window");
    let (at, size) = (light.absolute_position(), light.size());
    slint::LogicalPosition::new(at.x + size.width + 4.0, at.y + size.height / 2.0)
}

/// The pointer moved to `at`, and the fades given time to end.
fn hover_at(app: &App, at: slint::LogicalPosition) {
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerMoved { position: at });
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(300));
}

/// The traffic lights' glyphs light while the pointer is anywhere over the group, the gaps
/// between the lights included (.traffic-lights:hover), and go out away from it.
#[test]
fn the_traffic_lights_glyphs_stay_lit_across_the_gaps() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.show().unwrap();
    let lit = || {
        let glyphs: Vec<_> =
            ElementHandle::find_by_element_id(&app, "TrafficLight::glyph-path").collect();
        assert_eq!(glyphs.len(), 3, "three glyphs");
        glyphs.iter().all(|glyph| glyph.computed_opacity() > 0.99)
    };
    let gap = traffic_light_gap(&app);
    hover_at(&app, slint::LogicalPosition::new(gap.x + 200.0, gap.y));
    assert!(!lit(), "away from the group");
    hover_at(&app, slint::LogicalPosition::new(gap.x - 8.0, gap.y));
    assert!(lit(), "on a light");
    hover_at(&app, gap);
    assert!(lit(), "in the gap");
    app.hide().unwrap();
}

/// A double-click in a gap between the traffic lights maximizes, as on the rest of the title
/// bar; a single click does nothing. (A drag from the gap moving the window rests on
/// WindowMoveArea's filter: the testing window's move counter is behind Slint's internal
/// feature.)
#[test]
fn a_double_click_between_the_traffic_lights_maximizes() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.show().unwrap();
    let gap = traffic_light_gap(&app);

    click_at(&app, gap.x, gap.y);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1000));
    assert!(!app.window().is_maximized(), "a single click does nothing");
    click_at(&app, gap.x, gap.y);
    click_at(&app, gap.x, gap.y);
    assert!(app.window().is_maximized(), "a double-click maximizes");
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1000));
    click_at(&app, gap.x, gap.y);
    click_at(&app, gap.x, gap.y);
    assert!(!app.window().is_maximized(), "and restores");
    app.hide().unwrap();
}

/// Publications from another thread hop to the event loop, where the newest one is bound, and so
/// do a draft edit's answer and an Inspect's, through the window's wiring. The only test with a
/// threaded platform: Slint keeps one event loop proxy per process.
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
    // A draft edit sent, whose answer comes from the runtime's thread too.
    let edits = publisher.edits();
    let workers = crate::settings::DraftKey::Workers;
    edits
        .lock()
        .unwrap()
        .begin(Mode::Remux, workers, &Default::default());
    let answered =
        crate::settings::settle_later(app.as_weak(), Arc::clone(&edits), Mode::Remux, workers);
    let worker = std::thread::spawn(move || {
        for (seq, title) in [(1, "Older"), (2, "Frieren")] {
            let mut state = ViewState::new(None);
            state.seq = seq;
            state.remux.title = title.into();
            state.busy = true;
            publisher.state(&state);
        }
        answered(Ok(false));
        // Queued behind the hops, so the loop applies them first.
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop().unwrap();
    worker.join().unwrap();
    assert_eq!(app.get_remux_head().title, "Frieren");
    assert!(app.get_busy());
    assert!(
        !edits.lock().unwrap().has(Mode::Remux, workers),
        "the answer settled the edit on the event loop"
    );

    // Through the window's wiring, each mode's pick is answered for that mode (pr-p2-review #1):
    // `a` picked in Remux, then in Extract before Remux's answer; that answer drops Remux's mark
    // and leaves Extract's pick marked.
    use crate::intents::tests::{Episodes, NoJobs, NoSamples};
    use mkv_desktop_core::actor::{Core, Ports};
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let publisher = SlintPublisher::new(&app);
    let ports = Ports {
        inspector: Episodes,
        launcher: NoJobs,
        sampler: NoSamples,
    };
    let core = Core::spawn(runtime.handle().clone(), publisher.clone(), ports);
    crate::intents::wire(&app, core, runtime.handle().clone(), publisher);
    app.set_mode(Mode::Remux);
    app.set_remux_pending_inspect("a".into());
    app.invoke_inspect("a".into());
    app.set_mode(Mode::Extract);
    app.set_extract_pending_inspect("a".into());
    // The loop runs until a mark is dropped, or for 5 s.
    let answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (window, seen) = (app.as_weak(), Arc::clone(&answered));
    let poll = std::thread::spawn(move || {
        use std::sync::atomic::Ordering::Relaxed;
        for _ in 0..500 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if seen.load(Relaxed) {
                return;
            }
            let seen = Arc::clone(&seen);
            let _ = window.upgrade_in_event_loop(move |app| {
                if app.get_remux_pending_inspect().is_empty()
                    || app.get_extract_pending_inspect().is_empty()
                {
                    seen.store(true, Relaxed);
                    slint::quit_event_loop().unwrap();
                }
            });
        }
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop().unwrap();
    poll.join().unwrap();
    assert!(answered.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(app.get_remux_pending_inspect(), "", "Remux's answer");
    assert_eq!(app.get_extract_marked(), "a", "Extract keeps its own pick");
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
    let mut first = row("Frieren - 01.mkv");
    first.meta = "FLAC → Opus · 2 subtitles".into();
    state.remux.rows = Arc::new(vec![first, running]);
    state.remux.manifest_rev = 1;
    state.remux.empty = None;
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();

    // The pick reads its meta line too, as the webview's button reads its content.
    assert_eq!(
        only(&app, "Frieren - 01.mkv")
            .accessible_description()
            .as_deref(),
        Some("FLAC → Opus · 2 subtitles")
    );
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
    assert_eq!(app.get_remux_pending_inspect(), "Frieren - 02.mkv");

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
    // The empty card's button and the Library's, each named by its own text.
    let open_folder = || ["Open folder…", "+ Open folder…"].map(|label| only(&app, label));
    open_folder()
        .iter()
        .for_each(ElementHandle::invoke_accessible_default_action);
    assert_eq!(opened.get(), 2);
    // A batch locks both.
    app.set_busy(true);
    open_folder()
        .iter()
        .for_each(ElementHandle::invoke_accessible_default_action);
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
    only(&app, "+ Open folder…").invoke_accessible_default_action();
    assert_eq!(opened.get(), 3);
    app.hide().unwrap();
}

/// A path too long for the list's head elides its lead; the leaf keeps the width it has under a
/// short path.
#[test]
fn the_crumbs_elide_their_lead_before_the_leaf() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    app.show().unwrap();
    // The lead's width and the leaf's, under `lead`.
    let widths = |lead: &str| {
        let mut state = ViewState::new(None);
        state.remux.crumbs = vec![lead.into(), "Season 1".into()];
        apply(&app, &state);
        let lead = format!("{lead}{}", crate::queue::CRUMB_SEPARATOR);
        (
            only(&app, &lead).size().width,
            only(&app, "Season 1").size().width,
        )
    };
    let (_, fits) = widths("Anime");
    let (lead, leaf) = widths(&["Anime"; 100].join(" "));
    assert_eq!(leaf, fits, "the leaf keeps its width");
    assert!(lead > 0.0 && lead + leaf < 920.0, "the lead elides: {lead}");
    app.hide().unwrap();
}

/// A filter that matches no row says so; the rows stay in the model, hidden. One that matches
/// hides the reorder buttons, from Tab too.
#[test]
fn a_filter_with_no_match_says_so() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let publisher = SlintPublisher::new(&app);
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Frieren - 01.mkv"), row("Frieren - 02.mkv")]);
    state.remux.empty = None;
    publisher.show(&state);
    publisher.show_filter(Mode::Remux, QueueFilter::Failed);
    app.show().unwrap();
    assert_eq!(app.get_remux_head().filter, QueueFilter::Failed);
    only(&app, "No episodes match this filter.");
    assert_eq!(app.get_remux_rows().row_count(), 2);
    // A filter that shows the rows: the order cannot change, so Move up / Move down are out of
    // the accessibility tree, as style.css's `visibility: hidden` takes them out, and Tab passes
    // them.
    let movers = || ElementHandle::find_by_accessible_label(&app, "Move up").count();
    // Every control Tab reaches, from the row's pick round to it again.
    let walk = || {
        let start = "pick:remux:Frieren - 01.mkv";
        tab_to(&app, start);
        let mut reached = vec![];
        for _ in 0..400 {
            key(&app, slint::platform::Key::Tab);
            if focused(&app) == start {
                return reached;
            }
            reached.push(focused(&app));
        }
        panic!("Tab never came back to {start:?}");
    };
    let reorder = |reached: &[String]| {
        reached
            .iter()
            .any(|key| key.starts_with("up:") || key.starts_with("down:"))
    };
    publisher.show_filter(Mode::Remux, QueueFilter::Todo);
    only(&app, "Frieren - 01.mkv");
    assert!(!reorder(&walk()), "Tab passes them");
    assert_eq!(movers(), 0);
    // Nor does a programmatic focus (a closing dialog's) reach them.
    app.global::<Focus>()
        .invoke_restore("up:remux:Frieren - 01.mkv".into());
    frame(&app);
    assert_eq!(focused(&app), "pick:remux:Frieren - 01.mkv");
    publisher.show_filter(Mode::Remux, QueueFilter::All);
    assert_eq!(movers(), 2);
    assert!(reorder(&walk()), "Tab reaches them under All");
    app.hide().unwrap();
}

/// Each list marks its own pick, as the webview keeps one per list: a remux pick does not mark
/// Extract's row of the same id, and remux's answer leaves an Extract pick alone.
#[test]
fn each_mode_marks_its_own_pick() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let mut state = ViewState::new(None);
    for view in [&mut state.remux, &mut state.extract] {
        view.rows = Arc::new(vec![row("a"), row("b")]);
        view.empty = None;
    }
    apply(&app, &state);
    app.set_extract_head(mkv_anime_batch_slint_ui::QueueHead {
        inspected: "b".into(),
        ..app.get_extract_head()
    });
    app.show().unwrap();

    only(&app, "a").invoke_accessible_default_action();
    assert_eq!(app.get_remux_marked(), "a");
    only(&app, "Extract").invoke_accessible_default_action();
    assert_eq!(
        app.get_extract_marked(),
        "b",
        "Extract keeps its own inspected row"
    );

    only(&app, "a").invoke_accessible_default_action();
    crate::intents::inspect_answered(&app, Mode::Remux, "a");
    assert_eq!(
        app.get_extract_marked(),
        "a",
        "remux's answer is not Extract's"
    );
    crate::intents::inspect_answered(&app, Mode::Extract, "a");
    assert_eq!(app.get_extract_marked(), "b");
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
/// (a scan of another folder) or another folder's rows put its list back at the top.
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

    // Another folder's rows with no scanning card published before them (a cached folder
    // settles within one tick): still from the top.
    app.set_remux_scroll(-600.0);
    state.seq = 5;
    state.remux.rows = rows("e");
    state.remux.manifest_rev = 4;
    state.remux.directory = Some("/e".into());
    publisher.show(&state);
    assert_eq!(y("e05"), top);
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

/// Another folder whose rows the filter hides every one of starts at the top once a chip shows
/// them, not at the last folder's offset; so does a folder rescanned after its Scan failed card.
#[test]
fn rows_after_a_hidden_list_or_a_card_start_at_the_top() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let publisher = SlintPublisher::new(&app);
    // The last folder's rows all done, the next one's all to do.
    let mut state = three_releases();
    state.remux.rows = Arc::new(
        state
            .remux
            .rows
            .iter()
            .map(|row| Row {
                bucket: Some(Bucket::Done),
                ..row.clone()
            })
            .collect(),
    );
    publisher.show(&state);
    app.show().unwrap();
    let y = |id: &str| {
        only(&app, &format!("Include {id} in the batch"))
            .absolute_position()
            .y
    };
    let top = y("a00");
    publisher.show_filter(Mode::Remux, QueueFilter::Done);
    app.set_remux_scroll(-600.0);
    frame(&app);
    assert!(app.get_remux_scroll() < 0.0);

    let mut other = three_releases();
    other.seq = 1;
    other.remux.manifest_rev = 2;
    other.remux.directory = Some("/media/Other".into());
    publisher.show(&other);
    frame(&app);
    // The chip clicked, then its rows mapped, as the host does.
    only(&app, "All 0").invoke_accessible_default_action();
    publisher.show_filter(Mode::Remux, QueueFilter::All);
    frame(&app);
    assert_eq!(y("a00"), top, "the new folder's first row heads the list");

    // Scrolled, the Scan failed card, then the same folder's rows again.
    app.set_remux_scroll(-600.0);
    frame(&app);
    other.seq = 2;
    other.remux.scan = ScanView::Failed;
    publisher.show(&other);
    frame(&app);
    only(&app, "Scan failed");
    other.seq = 3;
    other.remux.scan = ScanView::Idle;
    other.remux.manifest_rev = 3;
    other.remux.rows = Arc::new(other.remux.rows.to_vec());
    publisher.show(&other);
    frame(&app);
    assert_eq!(y("a00"), top, "the rescanned rows start at the top");
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
    ] {
        only(&app, label);
    }
    // Below the fold over the bottom bar: the pane scrolls to them.
    scroll(&app, 640.0, false);
    for label in [
        "FONTS · 2",
        "2 attached fonts · folder: auto",
        "Assign subtitle",
    ] {
        only(&app, label);
    }
    scroll(&app, 640.0, true);
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

/// The Inspector's error can be selected and copied, as its `user-select: text`: a read-only
/// text input with the error as its value, wrapped in the pane at the narrowest window. A Tab
/// stop, it draws the ring for keyboard focus alone (`:focus-visible`).
#[test]
fn the_inspector_error_wraps_and_can_be_selected_and_copied() {
    use i_slint_backend_testing::AccessibleRole;
    use i_slint_core::platform::Clipboard;
    use i_slint_core::window::WindowInner;
    use slint::platform::{Key, WindowEvent};
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    app.show().unwrap();
    let error = "Inspection failed: /media/Some.Long.Release.Name.With.No.Spaces/Season.1/ep1.mkv";
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    let mut view = sample_view();
    view.error = Some(error.into());
    state.remux.inspector = Some(view);
    apply(&app, &state);

    let found: Vec<_> = ElementHandle::find_by_element_type_name(&app, "TextInput")
        .filter(|element| element.accessible_value().as_deref() == Some(error))
        .collect();
    assert_eq!(found.len(), 1, "one error field");
    let field = &found[0];
    assert_eq!(field.accessible_role(), Some(AccessibleRole::TextInput));
    assert_eq!(field.accessible_read_only(), Some(true));
    let (at, size) = (field.absolute_position(), field.size());
    assert!(
        at.x + size.width <= 760.0,
        "inside the pane: {at:?} {size:?}"
    );
    assert!(size.height > 2.0 * 12.0 * 1.5, "it wraps: {size:?}");

    // Focused by a click, Ctrl+A selects it all and Ctrl+C copies it.
    pointer(&app, field, true);
    pointer(&app, field, false);
    let chord = |text: &str| {
        let press = |text: slint::SharedString| {
            app.window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            app.window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        };
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        press(text.into());
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
    };
    chord("a");
    chord("c");
    let copied = WindowInner::from_pub(app.window())
        .context()
        .platform()
        .clipboard_text(Clipboard::DefaultClipboard);
    assert_eq!(copied.as_deref(), Some(error));

    // The ring goes around the error's box: none after the click, one once Tab reaches it, none
    // once Tab leaves it.
    let around = |&(x, y, width, height): &(f32, f32, f32, f32)| {
        x < at.x && y < at.y && x + width > at.x + size.width && y + height > at.y + size.height
    };
    assert!(!rings(&app).iter().any(around), "no ring after a click");
    for _ in 0..400 {
        if rings(&app).iter().any(around) {
            break;
        }
        key(&app, Key::Tab);
    }
    assert!(rings(&app).iter().any(around), "Tab draws the ring");
    key(&app, Key::Tab);
    assert!(
        !rings(&app).iter().any(around),
        "no ring once the focus has left"
    );
    app.hide().unwrap();
}

/// A release title too long for the list at the narrowest window wraps, as .release-title does,
/// a spaceless one too; it stays inside the list.
#[test]
fn a_long_release_title_wraps_at_the_narrowest_window() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    app.show().unwrap();
    let title = |title: &str| {
        let mut state = ViewState::new(None);
        state.remux.title = title.into();
        apply(&app, &state);
        let element = only(&app, title);
        let (at, size) = (element.absolute_position(), element.size());
        assert!(
            at.x + size.width <= 760.0 - 248.0,
            "inside the list: {size:?}"
        );
        size.height
    };
    let line = title("Frieren");
    for long in [
        "Sousou no Frieren Season 2 BDRip 1080p HEVC FLAC Dual Audio",
        "Sousou.no.Frieren.Season.2.BDRip.1080p.HEVC.FLAC.Dual.Audio",
    ] {
        let height = title(long);
        assert!(
            height > 1.5 * line,
            "{long:?} wraps: {height} against {line}"
        );
    }
    app.hide().unwrap();
}

/// The empty-state card takes the list's width less its 12px side padding, up to 340px
/// (`margin: 48px auto` in `.list { padding: 0 12px }`): 292px at 760px, where the list is
/// 760 - 196 (sidebar) - 248 (Inspector) wide.
#[test]
fn the_empty_card_follows_the_lists_side_padding() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let width = only(&app, "No episodes yet").size().width;
    assert!((width - 292.0).abs() < 0.5, "the card is {width}px wide");
    app.hide().unwrap();
}

/// The number fields and "Encoder complexity" show their label's `title` as a hover tooltip over
/// the whole field, its buttons and box included; the complexity box reads it as its description.
#[test]
fn the_number_fields_and_the_complexity_show_their_tooltips_on_hover() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window()
        .set_size(slint::LogicalSize::new(920.0, 1100.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    app.invoke_open_sheet(Mode::Remux, "".into());
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let limits = crate::settings::limits();
    let shown = |tip: &str| ElementHandle::find_by_accessible_label(&app, tip).count();
    let over = |label: &str| {
        let element = only(&app, label);
        let (at, size) = (element.absolute_position(), element.size());
        // In from outside the field, then the show delay.
        hover_at(&app, slint::LogicalPosition::new(1.0, 1.0));
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(
                    at.x + size.width / 2.0,
                    at.y + size.height / 2.0,
                ),
            });
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(2));
    };
    for (label, tip) in [
        ("Increase workers", &limits.workers_tip),
        ("Workers", &limits.workers_tip),
        ("Decrease disk I/O jobs", &limits.io_jobs_tip),
        ("FFmpeg threads", &limits.ffmpeg_threads_tip),
        ("Encoder complexity", &limits.complexity_tip),
        ("Encoder complexity value", &limits.complexity_tip),
    ] {
        assert_eq!(shown(tip), 0, "{label}: hidden before the hover");
        over(label);
        assert_eq!(shown(tip), 1, "{label}: shown on hover");
        hover_at(&app, slint::LogicalPosition::new(1.0, 1.0));
    }
    assert_eq!(
        only(&app, "Encoder complexity value").accessible_description(),
        Some(limits.complexity_tip.clone())
    );
    app.hide().unwrap();
}

/// The text input inside the field labelled `label`.
fn text_input_of(app: &App, label: &str) -> ElementHandle {
    let mut found = only(app, label)
        .query_descendants()
        .match_type_name("TextInput")
        .find_all();
    assert_eq!(found.len(), 1, "one text input in {label:?}");
    found.pop().unwrap()
}

/// A focused field with text shows no clear button, as the webview's `<input>` has none: nothing
/// a pointer can press appears in it, and the complexity box keeps its room for "10". The text
/// starts at `.field`'s inset. Each field reads as a text input with its value and placeholder.
#[test]
fn a_focused_field_shows_no_clear_button_and_reads_as_a_text_input() {
    use i_slint_backend_testing::AccessibleRole;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window()
        .set_size(slint::LogicalSize::new(920.0, 1100.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let mut form = app.get_remux_form();
    form.complexity = "10".into();
    app.set_remux_form(form);
    app.invoke_open_sheet(Mode::Remux, "".into());
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    tick();

    let label = "Encoder complexity value";
    let field = only(&app, label);
    assert_eq!(field.accessible_role(), Some(AccessibleRole::TextInput));
    assert_eq!(field.accessible_value().as_deref(), Some("10"));
    let presses = || {
        only(&app, label)
            .query_descendants()
            .match_type_name("TouchArea")
            .find_all()
            .len()
    };
    let room = || text_input_of(&app, label).size().width;
    let unfocused = room();
    let unfocused_presses = presses();
    // The text starts where `.field`'s does: inside its 1px border and 10px padding.
    let inset = text_input_of(&app, label).absolute_position().x - field.absolute_position().x;
    assert_eq!(inset, 11.0, "the text's inset");
    pointer(&app, &field, true);
    pointer(&app, &field, false);
    tick();
    assert_eq!(app.global::<Sheet>().get_focused(), "complexity");
    assert_eq!(
        presses(),
        unfocused_presses,
        "no clear button in the focused field"
    );
    assert_eq!(room(), unfocused, "the focus takes no room from the text");
    assert!(
        unfocused >= 24.0,
        "room for two digits at 12px: {unfocused}"
    );

    let folder = only(&app, "Fonts directory");
    assert_eq!(folder.accessible_role(), Some(AccessibleRole::TextInput));
    assert_eq!(folder.accessible_value().as_deref(), Some(""));
    assert_eq!(
        folder.accessible_placeholder_text().as_deref(),
        Some("Auto: fonts folder")
    );
    app.hide().unwrap();
}

/// A text longer than its field slides left as it is typed, so its end, with the caret, stays in
/// view; Home brings its start back, End its end, and a text cut back to fit sits at the start. A
/// press in the field's side padding focuses it, as anywhere in an `<input>`'s box.
#[test]
fn a_text_longer_than_its_field_keeps_the_caret_in_view() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window()
        .set_size(slint::LogicalSize::new(920.0, 1200.0));
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
    state.remux.rows = Arc::new(vec![row("Season 1/ep1.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(assigned);
    apply(&app, &state);
    app.show().unwrap();

    let field = only(&app, "Track title");
    let (at, size) = (field.absolute_position(), field.size());
    // Pressed in the padding, outside the text's clip: the keys below reach it only if it focused.
    click_at(&app, at.x + 4.0, at.y + size.height / 2.0);
    let typed = "W".repeat(40);
    for letter in typed.chars() {
        key(&app, letter.to_string());
    }
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    assert_eq!(app.get_sub_title(), typed.as_str());
    let text = || {
        let text = text_input_of(&app, "Track title");
        (text.absolute_position().x, text.size().width)
    };
    // The clip: the field less `.field`'s 11px inset on each side.
    let (left, right) = (at.x + 11.0, at.x + size.width - 11.0);
    let end_in_view = |(x, width): (f32, f32)| x < left && x + width <= right;
    assert!(text().1 > size.width, "longer than the field");
    assert!(
        end_in_view(text()),
        "its end in view: {:?} in {left}..{right}",
        text()
    );
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    key(&app, slint::platform::Key::Home);
    tick();
    assert_eq!(text().0, left, "Home shows its start");
    key(&app, slint::platform::Key::End);
    tick();
    assert!(end_in_view(text()), "End shows its end again: {:?}", text());
    for _ in 0..36 {
        key(&app, slint::platform::Key::Backspace);
    }
    tick();
    assert_eq!(app.get_sub_title(), "WWWW");
    assert_eq!(text().0, left, "a text that fits sits at the start");
    app.hide().unwrap();
}

/// The folder fields type in JetBrains Mono (.field.mono): a run of narrow letters is as wide as
/// any other, wider than the field, where Onest's would fit in it.
#[test]
fn the_folder_fields_type_in_jetbrains_mono() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window()
        .set_size(slint::LogicalSize::new(920.0, 1100.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let mut form = app.get_remux_form();
    form.input_dir = "i".repeat(80).into();
    app.set_remux_form(form);
    app.invoke_open_sheet(Mode::Remux, "".into());
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    // JetBrains Mono advances every glyph 0.6em; Onest's "i" is about a quarter of that.
    let width = text_input_of(&app, "Input folder").size().width;
    assert!(width > 80.0 * 12.0 * 0.6 * 0.9, "monospaced: {width}");
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
    let open_folder = || ElementHandle::find_by_accessible_label(&app, "+ Open folder…").count();
    assert_eq!(open_folder(), 0, "30 releases overflow the window");
    scroll(&app, 90.0, false);
    let open = only(&app, "+ Open folder…");
    let end = open.absolute_position().y + open.size().height;
    assert!(end <= 520.0, "Open folder ends at {end}, past the window");
    app.hide().unwrap();
}

/// A title typed and not committed is committed once before a screen reader's activation of a
/// custom control (a row, Open folder, a filter chip, a Library entry, a mode tab, Reset), as the
/// webview's blur commits it before the click lands; an answered Intent's write-back keeps what is
/// typed meanwhile.
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
    for click in ["+ Open folder…", "Done 0", "Show 2", "Extract"] {
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

/// Text typed into the subtitle fields and not committed stays with its episode: the same id in
/// another folder (the core keeps a folder's queue while the next one scans) gets its own
/// published fields, as does the episode once its assignment is cleared, and an Inspector that
/// shows no episode leaves nothing to commit.
#[test]
fn typed_subtitle_text_stays_with_its_folders_episode() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let publisher = SlintPublisher::new(&app);
    let mut assigned = sample_view();
    assigned.assignment = Some(mkv_core::request::TrackAssignment {
        target_video: "Season 1/ep1.mkv".into(),
        asset_path: "Season 1/ep1.ass".into(),
        track_type: Default::default(),
        language: Some("eng".into()),
        title: Some("Dialogue".into()),
        offset_ms: None,
    });
    let mut state = ViewState::new(None);
    state.remux.directory = Some("/media/A".into());
    state.remux.rows = Arc::new(vec![row("Season 1/ep1.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(assigned);
    publisher.show(&state);
    assert_eq!(app.get_sub_title(), "Dialogue");
    let type_signs = || {
        app.set_sub_title("Signs".into());
        app.set_sub_offset("-500".into());
        app.set_sub_dirty(true);
    };
    type_signs();
    crate::inspector::restore_form(&app);
    assert_eq!(app.get_sub_title(), "Signs", "kept on its own episode");

    state.seq = 1;
    state.remux.directory = Some("/media/B".into());
    publisher.show(&state);
    let fields = |app: &App| {
        (
            app.get_sub_title().to_string(),
            app.get_sub_offset().to_string(),
            app.get_sub_dirty(),
        )
    };
    assert_eq!(fields(&app), ("Dialogue".into(), "".into(), false));
    assert_eq!(app.get_remux_inspector().directory, "/media/B");

    // The episode still shown, its assignment cleared: nothing is left to send the typing to.
    type_signs();
    state.seq = 2;
    state.remux.inspector = Some(sample_view());
    publisher.show(&state);
    assert!(app.get_remux_inspector().shown);
    assert_eq!(
        fields(&app),
        ("".into(), "".into(), false),
        "no longer assigned"
    );

    type_signs();
    state.seq = 3;
    state.remux.inspector = None;
    publisher.show(&state);
    assert!(!app.get_sub_dirty(), "no episode, nothing to commit");
}

/// Reset removes itself when clicked: the window's keys keep working after it (Esc, Ctrl+T).
#[test]
fn the_window_keys_work_after_reset_removes_itself() {
    use slint::platform::{Key, WindowEvent};
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
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(assigned);
    apply(&app, &state);
    app.show().unwrap();
    // The core's answer: the assignment is gone, and Reset with it.
    let weak = app.as_weak();
    app.on_reset_subtitle(move || {
        let mut state = ViewState::new(None);
        state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
        state.remux.empty = None;
        state.remux.inspector = Some(sample_view());
        apply(&weak.upgrade().unwrap(), &state);
    });
    let toggles = Rc::new(Cell::new(0));
    let count = Rc::clone(&toggles);
    app.global::<Theme>()
        .on_toggle(move || count.set(count.get() + 1));

    let reset = only(&app, "Reset");
    pointer(&app, &reset, true);
    pointer(&app, &reset, false);
    assert_eq!(
        ElementHandle::find_by_accessible_label(&app, "Reset").count(),
        0
    );
    app.set_log_open(true);
    key(&app, Key::Escape);
    assert!(!app.get_log_open(), "Esc closed the drawer");
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    key(&app, "t");
    assert_eq!(toggles.get(), 1, "Ctrl+T toggled the theme");
    app.hide().unwrap();
}

/// A rescan that resets a mode's rows (every Run forces one) drops the row that had the focus;
/// the window's key scope takes it, so Esc still closes the drawer. A row of the other mode
/// keeps the focus.
#[test]
fn the_window_keys_work_after_a_reset_drops_the_focused_row() {
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv"), row("Show - 02.mkv")]);
    state.remux.empty = None;
    // One publisher, so a state whose remux rows did not change leaves their model alone.
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>()
        .invoke_restore("pick:remux:Show - 01.mkv".into());
    frame(&app);
    assert_eq!(focused(&app), "pick:remux:Show - 01.mkv");

    // An extract reset leaves the remux row its focus.
    let mut extract = state.clone();
    extract.seq += 1;
    extract.extract.rows = Arc::new(vec![row("Show - 01.mkv")]);
    extract.extract.empty = None;
    publisher.show(&extract);
    frame(&app);
    assert_eq!(focused(&app), "pick:remux:Show - 01.mkv");

    // A rescan's new rows reset the remux model.
    let mut rescan = extract.clone();
    rescan.seq += 1;
    rescan.remux.rows = Arc::new(vec![row("Show - 01.mkv"), row("Show - 03.mkv")]);
    publisher.show(&rescan);
    frame(&app);
    assert_eq!(focused(&app), "", "the dropped row's key is cleared");
    app.set_log_open(true);
    key(&app, Key::Escape);
    assert!(!app.get_log_open(), "Esc closed the drawer");
    app.hide().unwrap();
}

/// A status change that the filter no longer shows (a row done under To do, mid-run) hides the
/// row with the focus; the window's key scope takes it. A row hidden beside it leaves the focus.
#[test]
fn the_window_keys_work_after_a_filter_hides_the_focused_row() {
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut state = ViewState::new(None);
    let rows = vec![
        row("Show - 01.mkv"),
        row("Show - 02.mkv"),
        row("Show - 03.mkv"),
    ];
    state.remux.rows = Arc::new(rows.clone());
    state.remux.empty = None;
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    publisher.show_filter(Mode::Remux, QueueFilter::Todo);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>()
        .invoke_restore("pick:remux:Show - 02.mkv".into());
    frame(&app);
    assert_eq!(focused(&app), "pick:remux:Show - 02.mkv");
    let done = |mut row: Row| {
        row.bucket = Some(Bucket::Done);
        row
    };

    // Another row done: the focus stays.
    let mut other = state.clone();
    other.seq += 1;
    other.remux.rows = Arc::new(vec![
        done(rows[0].clone()),
        rows[1].clone(),
        rows[2].clone(),
    ]);
    publisher.show(&other);
    frame(&app);
    assert_eq!(focused(&app), "pick:remux:Show - 02.mkv");

    // The focused row done: the window's keys take it.
    let mut hidden = other.clone();
    hidden.seq += 1;
    hidden.remux.rows = Arc::new(vec![
        done(rows[0].clone()),
        done(rows[1].clone()),
        rows[2].clone(),
    ]);
    publisher.show(&hidden);
    frame(&app);
    assert_eq!(focused(&app), "");
    app.set_log_open(true);
    key(&app, Key::Escape);
    assert!(!app.get_log_open(), "Esc closed the drawer");
    app.hide().unwrap();
}

/// Esc closes the open drawer: the window's key scope hears it.
fn assert_window_keys(app: &App) {
    app.set_log_open(true);
    key(app, slint::platform::Key::Escape);
    assert!(!app.get_log_open(), "Esc closed the drawer");
}

/// A preset picked from the keyboard is published active: its row is written in place, so it
/// keeps the focus, and the window's keys still work.
#[test]
fn a_preset_picked_from_the_keyboard_keeps_the_focus() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let state = ViewState::new(None);
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>().invoke_restore("preset:fast".into());
    frame(&app);
    assert_eq!(focused(&app), "preset:fast");

    let mut picked = state.clone();
    picked.seq += 1;
    picked.remux.draft.preset = Some("fast".into());
    publisher.show(&picked);
    frame(&app);
    assert!(app
        .get_presets()
        .iter()
        .any(|row| row.id == "fast" && row.active));
    assert_eq!(focused(&app), "preset:fast");
    assert_window_keys(&app);
    app.hide().unwrap();
}

/// A Library whose counts change is written in place, so its focused entry keeps the focus; one
/// with fewer entries is replaced, and the window's key scope takes the focus.
#[test]
fn a_library_update_keeps_a_focused_entry_or_hands_the_focus_on() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let state = three_releases();
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>().invoke_restore("library:S2".into());
    frame(&app);
    assert_eq!(focused(&app), "library:S2");

    let mut rows = state.remux.rows.as_ref().clone();
    rows.push(Row {
        release: "S2".into(),
        ..row("b20")
    });
    let mut counted = state.clone();
    counted.seq += 1;
    counted.remux.library = presentation::library(Some("/media/Show"), &rows);
    counted.remux.rows = Arc::new(rows.clone());
    publisher.show(&counted);
    frame(&app);
    only(&app, "S2 21");
    assert_eq!(focused(&app), "library:S2");
    assert_window_keys(&app);

    // S3's rows gone: one entry fewer.
    rows.retain(|row| row.release != "S3");
    let mut fewer = counted.clone();
    fewer.seq += 1;
    fewer.remux.library = presentation::library(Some("/media/Show"), &rows);
    fewer.remux.rows = Arc::new(rows);
    publisher.show(&fewer);
    frame(&app);
    assert_eq!(focused(&app), "");
    assert_window_keys(&app);
    app.hide().unwrap();
}

/// A Library entry renamed in place keeps the focus, and `Focus.current` names it by its new key:
/// a stale key would outlive the focus and later take it from a field.
#[test]
fn a_library_entry_renamed_in_place_keeps_its_focus_key_current() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let state = three_releases();
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>().invoke_restore("library:S2".into());
    frame(&app);
    assert_eq!(focused(&app), "library:S2");

    // S2 becomes S4: as many entries, the second one renamed.
    let mut rows = state.remux.rows.as_ref().clone();
    for row in rows.iter_mut().filter(|row| row.release == "S2") {
        row.release = "S4".into();
    }
    let mut renamed = state.clone();
    renamed.seq += 1;
    renamed.remux.library = presentation::library(Some("/media/Show"), &rows);
    renamed.remux.rows = Arc::new(rows);
    publisher.show(&renamed);
    frame(&app);
    assert_eq!(focused(&app), "library:S4");
    assert_window_keys(&app);
    app.hide().unwrap();
}

/// Enter on the empty card's Open folder…, then the scan: the Scanning card has no button, and
/// the window's key scope takes the focus.
#[test]
fn the_window_keys_work_after_the_card_drops_its_focused_button() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let state = ViewState::new(None);
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>()
        .invoke_restore("remux:open-folder".into());
    frame(&app);
    assert_eq!(focused(&app), "remux:open-folder");

    let mut scanning = state.clone();
    scanning.seq += 1;
    scanning.remux.empty = None;
    scanning.remux.scan = ScanView::Scanning {
        directory: "/media/Show".into(),
    };
    publisher.show(&scanning);
    frame(&app);
    assert_eq!(focused(&app), "");
    assert_window_keys(&app);
    app.hide().unwrap();
}

/// A screen reader's expand on the Log toggle focuses it first, as a click does, and opens the
/// drawer.
#[test]
fn the_log_toggle_expand_action_takes_the_focus_first() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>().invoke_restore("preset:fast".into());
    frame(&app);
    assert_eq!(focused(&app), "preset:fast");

    only(&app, "Log").invoke_accessible_expand_action();
    assert_eq!(focused(&app), "Log");
    assert!(app.get_log_open());
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

/// What a screen reader reads in an open select's list: the label of the entry selected in the
/// one `list` whose entries mark a selection. The list's `accessible-delegate-focus` is not
/// checked: the testing backend has no getter for it.
fn heard(app: &App) -> Option<String> {
    use i_slint_backend_testing::AccessibleRole;
    i_slint_backend_testing::ElementQuery::from_root(app)
        .match_descendants()
        .match_accessible_role(AccessibleRole::List)
        .find_all()
        .into_iter()
        .find_map(|list| {
            let entries = list
                .query_descendants()
                .match_accessible_role(AccessibleRole::ListItem)
                .find_all();
            let selected = entries
                .iter()
                .find(|entry| entry.accessible_item_selected() == Some(true))?;
            assert_eq!(list.accessible_item_count(), Some(entries.len()));
            selected.accessible_label().map(String::from)
        })
}

/// A held key: one press, `repeats` repeats, then the release.
fn hold(app: &App, text: slint::platform::Key, repeats: usize) {
    use slint::platform::WindowEvent;
    let window = app.window();
    window.dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    for _ in 0..repeats {
        window.dispatch_event(WindowEvent::KeyPressRepeated { text: text.into() });
    }
    window.dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

/// The select labelled `label`.
fn select_of(app: &App, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, label)
        .find(|element| {
            element.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Combobox)
        })
        .unwrap_or_else(|| panic!("a {label:?} select"))
}

/// Drives the select labelled `label`, in a pane that overflows, as a user who scrolls or arrows
/// over it does, `sent` counting what it sent: a click opens its list and Esc closes it unchanged,
/// the focus back on it; the wheel over it scrolls the pane and sends nothing, as the arrows do,
/// closed or in its list, where a screen reader hears the entry highlighted; a held Return opens
/// the list without picking, and picks the entry highlighted once, the select having the focus
/// again, closed.
fn a_select_sends_a_pick_alone(app: &App, label: &str, sent: &dyn Fn() -> usize) {
    use slint::platform::{Key, WindowEvent};
    let select = select_of(app, label);
    let shown = select.accessible_value();
    pointer(app, &select, true);
    pointer(app, &select, false);
    frame(app);
    assert_eq!(
        select.accessible_expanded(),
        Some(true),
        "{label}: a click opens it"
    );
    key(app, Key::Escape);
    assert_eq!(
        select.accessible_expanded(),
        Some(false),
        "{label}: Esc closes it"
    );
    assert_eq!(focused(app), label, "{label}: the focus is back on it");

    let (at, size) = (select.absolute_position(), select.size());
    let position = slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
    for delta_y in [-40.0, 40.0, -40.0, -40.0] {
        app.window().dispatch_event(WindowEvent::PointerScrolled {
            position,
            delta_x: 0.0,
            delta_y,
        });
    }
    frame(app);
    assert!(
        select.absolute_position().y < at.y,
        "{label}: the wheel over it scrolls its pane"
    );
    key(app, Key::DownArrow);
    key(app, Key::UpArrow);
    assert_eq!(sent(), 0, "{label}: the wheel and the arrows send nothing");
    assert_eq!(select.accessible_value(), shown, "{label}: nor change it");

    // Return held on the closed select: the list opens on the press, and its repeats pick nothing.
    hold(app, Key::Return, 3);
    frame(app);
    assert_eq!(
        select.accessible_expanded(),
        Some(true),
        "{label}: Return opens it"
    );
    assert_eq!(sent(), 0, "{label}: a held Return opens it without a pick");
    assert_eq!(
        heard(app).as_deref(),
        shown.as_deref(),
        "{label}: its list is read from the entry shown"
    );
    key(app, Key::DownArrow);
    let next = heard(app);
    assert_ne!(
        next,
        shown.as_deref().map(String::from),
        "{label}: Down is heard"
    );
    for arrow in [Key::UpArrow, Key::DownArrow] {
        key(app, arrow);
    }
    assert_eq!(heard(app), next);
    assert_eq!(
        sent(),
        0,
        "{label}: the arrows in its list move the highlight only"
    );
    // Return held in the list: the press picks, and the repeats do not open it again.
    hold(app, Key::Return, 3);
    frame(app);
    assert_eq!(sent(), 1, "{label}: a pick sends once");
    assert_eq!(select.accessible_expanded(), Some(false));
    assert_eq!(
        select.accessible_value().map(String::from),
        next,
        "{label}: the entry heard is picked"
    );
}

/// The Inspector's selects change the Track Assignment on a pick alone: the wheel over them
/// scrolls the pane, and the arrows in their list only move the highlight, as a WebKitGTK
/// `<select>` does.
#[test]
fn the_inspector_selects_send_a_pick_alone() {
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
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(assigned);
    apply(&app, &state);
    app.show().unwrap();
    frame(&app);
    let calls = Rc::new(Cell::new(0));
    let count = Rc::clone(&calls);
    app.on_choose_subtitle(move |_| count.set(count.get() + 1));
    let count = Rc::clone(&calls);
    app.on_choose_language(move |_| count.set(count.get() + 1));

    for label in ["Assign subtitle", "Language"] {
        calls.set(0);
        // The Inspector back at its top: each select scrolls it.
        scroll(&app, 800.0, true);
        frame(&app);
        a_select_sends_a_pick_alone(&app, label, &|| calls.get());
    }
    app.hide().unwrap();
}

/// The sheet's bitrate select sends its edit on a pick alone, so the wheel over it never turns
/// the preset custom; Esc in its list closes the list, not the sheet.
#[test]
fn the_bitrate_select_sends_a_pick_alone() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    app.invoke_open_sheet(Mode::Remux, "".into());
    frame(&app);
    let calls = Rc::new(Cell::new(0));
    let count = Rc::clone(&calls);
    app.global::<Sheet>()
        .on_committed(move |_| count.set(count.get() + 1));
    a_select_sends_a_pick_alone(&app, "Opus bitrate", &|| calls.get());
    assert!(app.get_sheet_open());
    // The entry shown picked again sends nothing, as a `<select>` fires no change for it.
    let edits = Rc::new(Cell::new(0));
    let count = Rc::clone(&edits);
    app.global::<Sheet>()
        .on_edited(move |_| count.set(count.get() + 1));
    let select = select_of(&app, "Opus bitrate");
    key(&app, slint::platform::Key::Return);
    frame(&app);
    assert_eq!(select.accessible_expanded(), Some(true));
    key(&app, slint::platform::Key::Return);
    frame(&app);
    assert_eq!(select.accessible_expanded(), Some(false));
    assert_eq!(
        (calls.get(), edits.get()),
        (1, 0),
        "a pick of the entry shown"
    );
    app.hide().unwrap();
}

/// A select locked while its list is open (the Inspector's, as a batch starts): a pick that lands
/// before the list closes picks nothing, and the list closes.
#[test]
fn a_select_locked_while_open_closes_and_picks_nothing() {
    use i_slint_backend_testing::AccessibleRole;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(sample_view());
    apply(&app, &state);
    app.show().unwrap();
    frame(&app);
    let calls = Rc::new(Cell::new(0));
    let count = Rc::clone(&calls);
    app.on_choose_subtitle(move |_| count.set(count.get() + 1));
    let select = select_of(&app, "Assign subtitle");
    let shown = select.accessible_value();
    pointer(&app, &select, true);
    pointer(&app, &select, false);
    frame(&app);
    assert_eq!(select.accessible_expanded(), Some(true));
    let other = i_slint_backend_testing::ElementQuery::from_root(&app)
        .match_descendants()
        .match_accessible_role(AccessibleRole::ListItem)
        .find_all()
        .into_iter()
        .find(|entry| {
            entry.accessible_item_selectable() == Some(true) && entry.accessible_label() != shown
        })
        .expect("an entry of its list not shown");
    app.set_busy(true);
    other.invoke_accessible_default_action();
    assert_eq!(calls.get(), 0, "no pick while locked");
    frame(&app);
    assert_eq!(select.accessible_expanded(), Some(false), "the list closes");
    assert_eq!(select.accessible_value(), shown);
    app.hide().unwrap();
}

/// A select with no entry opens no list and picks nothing.
#[test]
fn a_select_with_no_entry_opens_nothing() {
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    apply(&app, &ViewState::new(None));
    app.set_bitrates(slint::ModelRc::default());
    app.show().unwrap();
    app.invoke_open_sheet(Mode::Remux, "".into());
    frame(&app);
    let calls = Rc::new(Cell::new(0));
    let count = Rc::clone(&calls);
    app.global::<Sheet>()
        .on_committed(move |_| count.set(count.get() + 1));
    let select = select_of(&app, "Opus bitrate");
    select.invoke_accessible_default_action();
    frame(&app);
    assert_eq!(select.accessible_expanded(), Some(false));
    for arrow in [Key::DownArrow, Key::Return] {
        key(&app, arrow);
    }
    frame(&app);
    assert_eq!(calls.get(), 0);
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
/// gradients' peaks), the placeholder hint on the fields, the glass panes over a full queue and
/// under their highlight, and each button's text over its fill: 4.5:1 (WCAG AA body text) in both
/// themes, with transparency reduced or not. Reads the tokens from the generated `Theme`,
/// the one place they are defined.
#[test]
fn every_text_token_meets_wcag_aa_on_every_surface_in_both_themes() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    let theme = app.global::<Theme>();
    let mut failures = Vec::new();
    for (dark, solid) in [(false, false), (true, false), (false, true), (true, true)] {
        theme.invoke_set_dark(dark);
        theme.set_reduce_transparency(solid);
        assert_eq!(theme.get_dark(), dark);
        let name = match (dark, solid) {
            (false, false) => "light",
            (true, false) => "dark",
            (false, true) => "light, reduced transparency",
            (true, true) => "dark, reduced transparency",
        };
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
            ("glass-float", theme.get_glass_float()),
            ("menu", theme.get_menu()),
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
        // A placeholder (hint) is drawn on the sheet's fields (field on float) and the
        // Inspector's (field on panel).
        for (wall, below) in walls {
            for (surface, fill) in [
                ("float", theme.get_float()),
                ("glass-float", theme.get_glass_float()),
                ("panel", theme.get_panel()),
            ] {
                let field = over(theme.get_field(), over(fill, below));
                let on = format!("field on {surface} over {wall}");
                pairs.push(("hint", on, theme.get_hint(), field));
            }
        }
        // The glass panes (GlassPane), under their top highlight at its strongest. The faked
        // float over a full queue behind, sharp: any text colour under it. The real blur's
        // glass-float over what a σ30 blur leaves of the queue behind a pane: the wallpaper, the
        // panes over it and their tints. Text and the small solid shapes (rings, checkboxes)
        // average out into those, and no large solid fill sits behind a pane (the bar's buttons
        // are below the drawer); the modal dimmer is, behind the sheet and a dialog.
        let sharp: Vec<(String, Color)> = texts
            .iter()
            .chain(&walls)
            .map(|&(name, color)| (name.to_string(), color))
            .collect();
        let mut blurred = Vec::new();
        for (wall, below) in walls {
            blurred.push((wall.to_string(), below));
            for &(surface, fill) in surfaces.iter().filter(|(name, _)| !name.ends_with("float")) {
                let pane = over(fill, below);
                blurred.push((format!("{surface} over {wall}"), pane));
                for (tint, tinted) in tints {
                    blurred.push((
                        format!("{tint} on {surface} over {wall}"),
                        over(tinted, pane),
                    ));
                }
            }
        }
        // The sheet and a dialog sit under the modal dimmer, the dialog over the sheet under two;
        // the faked float and glass-float-dim (the panes' `dimmed`) are judged over those too.
        let dimmed = |behind: &[(String, Color)], times: usize| {
            let mut out = behind.to_vec();
            for _ in 0..times {
                for (name, color) in &mut out {
                    *name = format!("dim over {name}");
                    *color = over(theme.get_dim(), *color);
                }
            }
            out
        };
        let sharp_all = [sharp.clone(), dimmed(&sharp, 1), dimmed(&sharp, 2)].concat();
        let blurred_dim = [dimmed(&blurred, 1), dimmed(&blurred, 2)].concat();
        // The tints drawn on a pane, with the sheet's lock note (warn at 14%, settings.slint).
        let pane_tints: Vec<(&str, Color)> = tints
            .iter()
            .copied()
            .chain([("warn 14%", theme.get_warn().transparentize(0.86))])
            .collect();
        for (fill, float, behind) in [
            ("float", theme.get_float(), &sharp_all),
            ("glass-float", theme.get_glass_float(), &blurred),
            ("glass-float-dim", theme.get_glass_float_dim(), &blurred_dim),
        ] {
            for (under, below) in behind {
                let pane = over(float, *below);
                let mut backgrounds = vec![
                    (format!("{fill} over {under}"), pane),
                    (
                        format!("glass-hi on {fill} over {under}"),
                        over(theme.get_glass_hi(), pane),
                    ),
                ];
                for (tint, tinted) in &pane_tints {
                    backgrounds.push((
                        format!("{tint} on {fill} over {under}"),
                        over(*tinted, pane),
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

/// A key pressed and released in the window.
fn key(app: &App, text: impl Into<slint::SharedString>) {
    use slint::platform::WindowEvent;
    let text = text.into();
    app.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    app.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

/// The pointer pressed, or released, at `element`'s centre.
pub(crate) fn pointer(app: &App, element: &ElementHandle, pressed: bool) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let (at, size) = (element.absolute_position(), element.size());
    let position = slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
    let button = PointerEventButton::Left;
    app.window().dispatch_event(if pressed {
        WindowEvent::PointerPressed { position, button }
    } else {
        WindowEvent::PointerReleased { position, button }
    });
}

/// The settings sheet, found by its label: the gear that opens it is labelled the same.
fn sheet_panel(app: &App, label: &str) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, label).find(|element| {
        element.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Groupbox)
    })
}

/// Each gear opens its mode's sheet; Done, a click on the backdrop and Esc (from a text field
/// too) close it, and the gear that opened it has the focus again: Enter opens it once more. The
/// sheet fits the narrowest window, its body scrolling.
#[test]
fn the_sheet_opens_from_its_gear_closes_every_way_and_gives_the_focus_back() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let gear = |label: &str| {
        ElementHandle::find_by_accessible_label(&app, label)
            .find(|element| {
                element.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
            })
            .unwrap()
    };

    gear("Extract settings").invoke_accessible_default_action();
    tick();
    assert!(app.get_sheet_open());
    assert_eq!(app.get_mode(), Mode::Extract);
    let panel = sheet_panel(&app, "Extract settings").expect("the extract sheet shows");
    assert!(
        panel.absolute_position().y + panel.size().height <= 520.0,
        "it fits the window"
    );
    only(&app, "Done").invoke_accessible_default_action();
    tick();
    assert!(!app.get_sheet_open());
    // The gear has the focus back: Enter opens the sheet again.
    key(&app, slint::platform::Key::Return);
    tick();
    assert!(app.get_sheet_open(), "Enter on the focused gear opens it");
    assert_eq!(app.get_mode(), Mode::Extract);

    // Esc, with Done focused as the sheet opens.
    key(&app, slint::platform::Key::Escape);
    tick();
    assert!(!app.get_sheet_open());

    // A click on the backdrop closes it; one on the sheet does not.
    gear("Remux settings").invoke_accessible_default_action();
    tick();
    assert_eq!(app.get_mode(), Mode::Remux);
    let panel = sheet_panel(&app, "Remux settings").unwrap();
    pointer(&app, &panel, true);
    pointer(&app, &panel, false);
    tick();
    assert!(app.get_sheet_open(), "a click on the sheet keeps it open");
    // Bottom left, beside the sheet.
    for pressed in [true, false] {
        use slint::platform::{PointerEventButton, WindowEvent};
        let (position, button) = (
            slint::LogicalPosition::new(20.0, 505.0),
            PointerEventButton::Left,
        );
        app.window().dispatch_event(if pressed {
            WindowEvent::PointerPressed { position, button }
        } else {
            WindowEvent::PointerReleased { position, button }
        });
    }
    tick();
    assert!(!app.get_sheet_open(), "a click on the backdrop closes it");

    // Esc from a text field: the field is left (an input folder left rescans, P4), then closed.
    app.invoke_open_sheet(Mode::Remux, "input_dir".into());
    tick();
    tick();
    let sheet = app.global::<Sheet>();
    assert_eq!(sheet.get_focused(), "input_dir");
    let left = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = Rc::clone(&left);
    sheet.on_left(move |key, _| log.borrow_mut().push(key.into()));
    key(&app, slint::platform::Key::Escape);
    tick();
    assert!(!app.get_sheet_open());
    assert_eq!(sheet.get_focused(), "");
    assert_eq!(left.borrow().first().map(String::as_str), Some("input_dir"));
    app.hide().unwrap();
}

/// A stepper's − and + step on a pointer press and stop on its release, take the focus, and step
/// once on Enter, Space or a screen reader's action; every control is labelled.
#[test]
fn the_steppers_answer_the_pointer_the_keyboard_and_a_screen_reader() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window()
        .set_size(slint::LogicalSize::new(920.0, 1100.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    app.invoke_open_sheet(Mode::Remux, "".into());
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    tick();
    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let sheet = app.global::<Sheet>();
    let log = Rc::clone(&calls);
    sheet.on_step_pressed(move |key, up| log.borrow_mut().push(format!("press {key} {up}")));
    let log = Rc::clone(&calls);
    sheet.on_step_released(move || log.borrow_mut().push("release".into()));
    let log = Rc::clone(&calls);
    sheet.on_step_key(move |key, up| log.borrow_mut().push(format!("key {key} {up}")));

    let plus = only(&app, "Increase workers");
    pointer(&app, &plus, true);
    pointer(&app, &plus, false);
    tick();
    key(&app, slint::platform::Key::Return);
    key(&app, " ");
    // A screen reader's step takes the focus first, as a press does: the field is left, so the
    // stepped value is written into it.
    app.invoke_open_sheet(Mode::Remux, "sub_offset".into());
    for _ in 0..3 {
        tick();
    }
    assert_eq!(sheet.get_focused(), "sub_offset");
    only(&app, "Decrease offset").invoke_accessible_default_action();
    tick();
    assert_eq!(sheet.get_focused(), "");
    assert_eq!(focused(&app), "step:sub_offset-");
    let calls = calls.borrow();
    assert_eq!(calls[0], "press workers true");
    assert!(calls.contains(&"release".to_string()));
    let keys: Vec<_> = calls
        .iter()
        .filter(|call| call.starts_with("key"))
        .collect();
    assert_eq!(
        keys,
        [
            "key workers true",
            "key workers true",
            "key sub_offset false"
        ]
    );

    for label in [
        "Input folder",
        "Output directory",
        "Fonts directory",
        "Assets directory",
        "Browse for the Input folder",
        "Opus bitrate",
        "Subtitle offset (seconds)",
        "Increase offset",
        "Encoder complexity",
        "Encoder complexity value",
        "Workers",
        "Decrease workers",
        "Disk I/O",
        "Increase disk I/O jobs",
        "FFmpeg threads",
        "Decrease FFmpeg threads",
        "Dry run (plan only, write nothing)",
        "Bypass inspection cache",
        "Done",
    ] {
        assert!(
            ElementHandle::find_by_accessible_label(&app, label).count() >= 1,
            "{label:?} is labelled"
        );
    }
    app.hide().unwrap();
}

/// Tab and Shift+Tab stay inside the open sheet, wrapping between Done and its last control, so
/// Esc still closes it; keys typed into a field reach the form and are sent as edits, in order.
#[test]
fn the_sheet_keeps_the_focus_and_its_fields_take_typed_keys() {
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let sheet = app.global::<Sheet>();
    let edited = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = Rc::clone(&edited);
    sheet.on_edited(move |key| log.borrow_mut().push(key.into()));

    app.invoke_open_sheet(Mode::Remux, "".into());
    tick();
    tick();
    // Done has the focus: Tab moves on to the input folder.
    key(&app, Key::Tab);
    tick();
    assert_eq!(sheet.get_focused(), "input_dir");
    // Back to Done, then back once more wraps to the last control, not out of the sheet.
    let back = |app: &App| key(app, Key::Backtab);
    back(&app);
    tick();
    assert_eq!(sheet.get_focused(), "");
    // Back from Done wraps to the last control, Clear inspection cache (the reduced effects moved
    // to the Appearance menu), and on to the one before it (without the wrap, the backdrop and
    // then the window behind it): Esc still closes the sheet.
    back(&app);
    tick();
    assert_eq!(focused(&app), "Clear inspection cache…");
    for effect in [
        "Reduce transparency (solid panes)",
        "Reduce motion (fewer animations)",
    ] {
        assert_eq!(
            ElementHandle::find_by_accessible_label(&app, effect).count(),
            0,
            "the sheet shows no {effect:?}"
        );
    }
    back(&app);
    tick();
    key(&app, Key::Escape);
    tick();
    assert!(!app.get_sheet_open(), "the focus stayed inside the sheet");
    // From the last control, Tab wraps to Done, and on to the input folder.
    app.invoke_open_sheet(Mode::Remux, "".into());
    tick();
    tick();
    back(&app);
    tick();
    key(&app, Key::Tab);
    tick();
    key(&app, Key::Tab);
    tick();
    assert_eq!(
        sheet.get_focused(),
        "input_dir",
        "the focus wrapped inside the sheet"
    );

    // Typed keys reach the form, each an edit.
    key(&app, "/");
    key(&app, "a");
    tick();
    assert_eq!(app.get_remux_form().input_dir, "/a");
    assert_eq!(*edited.borrow(), ["input_dir", "input_dir"]);

    // Esc still closes it.
    key(&app, Key::Escape);
    tick();
    assert!(!app.get_sheet_open());
    app.hide().unwrap();
}

/// The theme toggle and Ctrl+T (from the window, and from a field of the open sheet) flip the
/// theme and save the choice; a saved choice applies before the window shows.
#[test]
fn the_toggle_and_ctrl_t_flip_the_theme_and_save_the_choice() {
    use crate::appearance::{self, Appearance};
    use crate::renderer::Renderer;
    use slint::platform::{Key, WindowEvent};
    i_slint_backend_testing::init_no_event_loop();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("appearance"), "theme=light\n").unwrap();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    let theme = app.global::<Theme>();
    theme.invoke_set_dark(true);
    appearance::wire(&app, Some(dir.path().into()), Renderer::OpenGl);
    // Applied before the first frame.
    assert!(!theme.get_dark());
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let saved = || Appearance::load(dir.path()).dark;

    only(&app, "Toggle theme").invoke_accessible_default_action();
    assert!(theme.get_dark());
    assert_eq!(saved(), Some(true));

    // Space held on the focused toggle flips it once, as a browser's button activates once.
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Space.into(),
    });
    for _ in 0..3 {
        app.window().dispatch_event(WindowEvent::KeyPressRepeated {
            text: Key::Space.into(),
        });
    }
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Space.into(),
    });
    assert!(!theme.get_dark(), "one flip for a held Space");
    key(&app, Key::Space);
    assert!(theme.get_dark());
    // A held Return flips it once too: the window would flip at the key's repeat rate.
    app.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Return.into(),
    });
    for _ in 0..3 {
        app.window().dispatch_event(WindowEvent::KeyPressRepeated {
            text: Key::Return.into(),
        });
    }
    app.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Return.into(),
    });
    assert!(!theme.get_dark(), "one flip for a held Return");
    key(&app, Key::Return);
    assert!(theme.get_dark());

    let ctrl_t = |app: &App| {
        let press = |text: slint::SharedString| {
            app.window()
                .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            app.window()
                .dispatch_event(WindowEvent::KeyReleased { text });
        };
        app.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        press("t".into());
        app.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
    };
    ctrl_t(&app);
    assert!(!theme.get_dark());
    assert_eq!(saved(), Some(false));
    // A plain t toggles nothing.
    key(&app, "t");
    assert!(!theme.get_dark());

    // From the input folder field of the open sheet: the field takes no "t".
    app.invoke_open_sheet(Mode::Remux, "input_dir".into());
    for _ in 0..2 {
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    }
    assert_eq!(app.global::<Sheet>().get_focused(), "input_dir");
    ctrl_t(&app);
    assert!(theme.get_dark());
    assert_eq!(saved(), Some(true));
    assert_eq!(app.get_remux_form().input_dir, "");
    app.hide().unwrap();
}

/// The Appearance menu's two reduced effects apply at once and are saved with the theme: Reduce
/// transparency solidifies the panes, Reduce motion stops the spinners. The real blur is off
/// (`appearance::REAL_BLUR`), so OpenGL draws the faked glass (a translucent float) as the software
/// renderer does; the panes still follow `Theme.glass` (the Log drawer's blur reads it).
#[test]
fn the_reduced_effects_apply_at_once_and_are_saved() {
    use crate::appearance::{self, Appearance};
    use crate::renderer::Renderer;
    i_slint_backend_testing::init_no_event_loop();
    let dir = tempfile::tempdir().unwrap();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    appearance::wire(&app, Some(dir.path().into()), Renderer::OpenGl);
    let theme = app.global::<Theme>();
    assert!(!theme.get_glass());
    assert!(theme.get_float().alpha() < 255, "the faked glass");
    assert_eq!(app.get_log_blur(), 0.0);
    // The panes follow `Theme.glass`: set, the drawer draws the real blur.
    theme.set_glass(true);
    assert!(app.get_log_blur() > 0.0);
    theme.set_glass(false);
    apply(&app, &ViewState::new(None));
    app.show().unwrap();

    appearance_button(&app).invoke_accessible_default_action();
    assert!(app.get_appearance_open());
    only(&app, "Reduce transparency (solid panes)").invoke_accessible_default_action();
    assert!(theme.get_reduce_transparency());
    assert!(!theme.get_glass());
    assert_eq!(theme.get_float().alpha(), 255);
    assert_eq!(app.get_log_blur(), 0.0);
    only(&app, "Reduce motion (fewer animations)").invoke_accessible_default_action();
    assert!(theme.get_reduce_motion());
    assert_eq!(
        Appearance::load(dir.path()),
        Appearance {
            dark: None,
            reduce_transparency: true,
            reduce_motion: true,
        }
    );
    only(&app, "Reduce transparency (solid panes)").invoke_accessible_default_action();
    assert!(theme.get_float().alpha() < 255, "the faked glass again");
    app.hide().unwrap();

    // A restart on the software renderer keeps the saved effects and draws no glass.
    let app = new_app();
    appearance::wire(&app, Some(dir.path().into()), Renderer::Software);
    let theme = app.global::<Theme>();
    assert!(theme.get_reduce_motion() && !theme.get_reduce_transparency());
    assert!(!theme.get_glass());
    assert!(theme.get_float().alpha() < 255, "the faked glass");
    assert_eq!(app.get_log_blur(), 0.0);
}

/// The title bar's Appearance button (its menu is labelled the same).
fn appearance_button(app: &App) -> ElementHandle {
    ElementHandle::find_by_accessible_label(app, "Appearance")
        .find(|element| {
            element.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Button)
        })
        .expect("the Appearance button")
}

/// The Appearance menu, if open.
fn appearance_menu(app: &App) -> Option<ElementHandle> {
    ElementHandle::find_by_accessible_label(app, "Appearance").find(|element| {
        element.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Groupbox)
    })
}

/// A left click at `x`, `y` in the window.
fn click_at(app: &App, x: f32, y: f32) {
    use slint::platform::{PointerEventButton, WindowEvent};
    let (position, button) = (slint::LogicalPosition::new(x, y), PointerEventButton::Left);
    app.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    app.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
}

/// The Appearance menu opens from its button, by a click or Enter, under it and right-aligned to
/// it in the narrowest window, with the focus on the current theme; Tab and Shift+Tab wrap
/// inside it. Esc, a click outside and the button close it and give the focus back to the
/// button, ringed after a key alone; a dialog opening closes it first; Esc closes it before the
/// Log drawer.
#[test]
fn the_appearance_menu_opens_from_its_button_and_closes_every_way() {
    use mkv_anime_batch_slint_ui::DialogTexts;
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let button = appearance_button(&app);
    assert_eq!(
        button.accessible_description().as_deref(),
        Some("Appearance")
    );
    assert!(appearance_menu(&app).is_none());

    // A click opens it, under the button and right-aligned to it, inside the window.
    pointer(&app, &button, true);
    pointer(&app, &button, false);
    tick();
    assert!(app.get_appearance_open());
    let menu = appearance_menu(&app).expect("the menu shows");
    let (at, size) = (menu.absolute_position(), menu.size());
    let (button_at, button_size) = (button.absolute_position(), button.size());
    assert_eq!(at.x + size.width, button_at.x + button_size.width);
    assert!(at.y >= button_at.y + button_size.height);
    assert!(
        at.x >= 0.0 && at.y + size.height <= 520.0,
        "inside the window"
    );
    assert!(
        focused(&app).starts_with("theme:"),
        "the theme choice has it"
    );
    assert!(rings(&app).is_empty(), "opened by the pointer: no ring");

    // Tab and Shift+Tab wrap inside it: the three choices, then the two checkboxes.
    let first = focused(&app);
    key(&app, Key::Backtab);
    key(&app, Key::Tab);
    assert_eq!(
        focused(&app),
        "theme:System",
        "Tab past the last wraps to the first"
    );
    for _ in 0..5 {
        key(&app, Key::Tab);
    }
    assert_eq!(focused(&app), "theme:System", "five stops, wrapping");
    assert!(app.get_appearance_open());

    // Esc closes it: the button has the focus back, ringed; Enter opens it again.
    key(&app, Key::Escape);
    tick();
    assert!(!app.get_appearance_open());
    assert!(appearance_menu(&app).is_none());
    assert_eq!(focused(&app), "Appearance");
    assert_eq!(rings(&app).len(), 1);
    key(&app, Key::Return);
    tick();
    assert!(app.get_appearance_open(), "Enter on the button opens it");
    assert_eq!(focused(&app), first);

    // A click outside closes it, the button focused without a ring.
    click_at(&app, 20.0, 505.0);
    tick();
    assert!(!app.get_appearance_open());
    assert_eq!(focused(&app), "Appearance");
    assert!(rings(&app).is_empty());

    // The button again closes it.
    for _ in 0..2 {
        pointer(&app, &button, true);
        pointer(&app, &button, false);
        tick();
    }
    assert!(!app.get_appearance_open(), "the second click closes it");
    assert_eq!(focused(&app), "Appearance");

    // A dialog opening closes it; the button has the focus back after the dialog.
    key(&app, Key::Return);
    tick();
    assert!(app.get_appearance_open());
    app.set_dialog(DialogTexts {
        title: "Notice".into(),
        text: "Something happened".into(),
        confirm: "OK".into(),
        notice: true,
    });
    app.invoke_show_dialog();
    tick();
    assert!(!app.get_appearance_open());
    assert_eq!(focused(&app), "OK");
    app.set_dialog_open(false);
    tick();
    tick();
    assert_eq!(focused(&app), "Appearance");

    // Over the open Log drawer, Esc closes the menu alone.
    app.set_log_open(true);
    key(&app, Key::Return);
    tick();
    key(&app, Key::Escape);
    tick();
    assert!(!app.get_appearance_open());
    assert!(app.get_log_open(), "the drawer stays open");
    app.hide().unwrap();
}

/// The menu's System, Light and Dark set the theme and save it: System saves no theme and
/// follows the system's scheme live again; the toggle and Ctrl+T still pin the other one, and the
/// menu's choice follows them while open. A saved System applies as the window starts.
#[test]
fn the_appearance_menu_picks_system_light_or_dark_and_saves_it() {
    use crate::appearance::{self, Appearance};
    use crate::renderer::Renderer;
    use i_slint_core::items::ColorScheme;
    use i_slint_core::window::WindowInner;
    i_slint_backend_testing::init_no_event_loop();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("appearance"), "theme=dark\n").unwrap();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    appearance::wire(&app, Some(dir.path().into()), Renderer::Software);
    let theme = app.global::<Theme>();
    assert!(theme.get_dark() && !theme.get_follows_system());
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let saved = || Appearance::load(dir.path()).dark;
    let system = |scheme| {
        WindowInner::from_pub(app.window())
            .context()
            .set_color_scheme(scheme)
    };
    let choice = |label: &str| {
        ElementHandle::find_by_accessible_label(&app, label)
            .find(|element| {
                element.accessible_role()
                    == Some(i_slint_backend_testing::AccessibleRole::RadioButton)
            })
            .unwrap_or_else(|| panic!("the {label:?} choice"))
    };
    let checked = || {
        ["System", "Light", "Dark"]
            .into_iter()
            .filter(|label| choice(label).accessible_checked() == Some(true))
            .collect::<Vec<_>>()
    };

    appearance_button(&app).invoke_accessible_default_action();
    assert_eq!(checked(), ["Dark"]);
    choice("Light").invoke_accessible_default_action();
    assert!(!theme.get_dark());
    assert_eq!(saved(), Some(false));
    assert_eq!(checked(), ["Light"]);

    // System: the system's scheme, live, saved as no theme.
    system(ColorScheme::Dark);
    assert!(!theme.get_dark(), "Light stays pinned");
    choice("System").invoke_accessible_default_action();
    assert!(theme.get_follows_system());
    assert!(theme.get_dark(), "the system's dark");
    assert_eq!(saved(), None);
    assert_eq!(checked(), ["System"]);
    system(ColorScheme::Light);
    assert!(!theme.get_dark(), "followed live");
    system(ColorScheme::Dark);
    assert!(theme.get_dark());

    // The toggle still pins the other theme in one click; the open menu follows it.
    only(&app, "Toggle theme").invoke_accessible_default_action();
    assert!(!theme.get_dark() && !theme.get_follows_system());
    assert_eq!(saved(), Some(false));
    assert_eq!(checked(), ["Light"]);
    choice("Dark").invoke_accessible_default_action();
    assert!(theme.get_dark());
    assert_eq!(saved(), Some(true));
    choice("System").invoke_accessible_default_action();
    app.hide().unwrap();

    // A restart with System saved follows the system's from the start, the menu saying so.
    let app = new_app();
    appearance::wire(&app, Some(dir.path().into()), Renderer::Software);
    let theme = app.global::<Theme>();
    assert!(theme.get_follows_system());
    WindowInner::from_pub(app.window())
        .context()
        .set_color_scheme(ColorScheme::Dark);
    assert!(theme.get_dark());
}

/// The `Focus` key of the custom control with the focus; empty for none of them.
pub(crate) fn focused(app: &App) -> String {
    app.global::<Focus>().get_current().into()
}

/// Presses Tab until the custom control of `key` has the focus.
pub(crate) fn tab_to(app: &App, key: &str) {
    for _ in 0..400 {
        self::key(app, slint::platform::Key::Tab);
        if focused(app) == key {
            return;
        }
    }
    panic!("Tab never reached {key:?}");
}

/// The focus rings drawn, each as its top left corner and size.
fn rings(app: &App) -> Vec<(f32, f32, f32, f32)> {
    ElementHandle::find_by_element_type_name(app, "FocusRing")
        .map(|ring| {
            let (at, size) = (ring.absolute_position(), ring.size());
            (at.x, at.y, size.width, size.height)
        })
        .collect()
}

/// Tab walks the custom controls in their visual order: the title bar, the sidebar, the list's
/// filters and rows, then the bottom bar; a row control's focus shows its Move up / Move down.
/// Enter and Space activate each kind of control.
#[test]
fn tab_walks_the_custom_controls_in_order_and_enter_or_space_activates_them() {
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    SlintPublisher::new(&app).show(&three_releases());
    app.show().unwrap();
    frame(&app);

    let mut walked = Vec::new();
    while walked.last().is_none_or(|last: &String| last != "Log") {
        key(&app, Key::Tab);
        walked.push(focused(&app));
        assert!(walked.len() < 400, "Tab reaches the Log toggle: {walked:?}");
        if focused(&app) == "check:remux:a00" {
            let shown = ElementHandle::find_by_accessible_label(&app, "Move up")
                .filter(|up| up.computed_opacity() > 0.99)
                .count();
            assert_eq!(shown, 1, "the focused row's Move up shows, no other's");
        }
    }
    let order = [
        "Close window",
        "Minimize window",
        "Maximize window",
        "Toggle theme",
        "Appearance",
        "Remux",
        "Remux settings",
        "Extract",
        "Extract settings",
        "library:/",
        "library:S2",
        "library:S3",
        "nav:open-folder",
        "remux:All",
        "remux:To do",
        "remux:Done",
        "remux:Failed",
        "remux:Select all",
        "remux:None",
        "check:remux:a00",
        "pick:remux:a00",
        "up:remux:a00",
        "down:remux:a00",
        "check:remux:a01",
        "run",
        "Log",
    ];
    let at: Vec<usize> = order
        .iter()
        .map(|key| {
            walked
                .iter()
                .position(|walked| walked == key)
                .unwrap_or_else(|| panic!("{key:?} takes the focus: {walked:?}"))
        })
        .collect();
    assert!(at.is_sorted(), "in visual order: {walked:?}");
    assert!(
        !walked.iter().any(|key| key.contains("extract:")),
        "the hidden pane takes no focus"
    );

    let calls = Rc::new(RefCell::new(Vec::<String>::new()));
    let log = Rc::clone(&calls);
    app.on_close_window(move || log.borrow_mut().push("close".into()));
    let log = Rc::clone(&calls);
    app.on_set_filter(move |filter| log.borrow_mut().push(format!("filter {filter:?}")));
    let log = Rc::clone(&calls);
    app.on_set_selected(move |id, on| log.borrow_mut().push(format!("select {id} {on}")));
    let log = Rc::clone(&calls);
    app.on_inspect(move |id| log.borrow_mut().push(format!("inspect {id}")));
    let log = Rc::clone(&calls);
    app.on_reveal_release(move |release, _| log.borrow_mut().push(format!("reveal {release}")));
    let log = Rc::clone(&calls);
    app.on_open_folder(move || log.borrow_mut().push("open".into()));
    for (to, press) in [
        ("Close window", Key::Return),
        ("library:S2", Key::Space),
        ("nav:open-folder", Key::Return),
        ("remux:Done", Key::Space),
        ("check:remux:a00", Key::Space),
        ("pick:remux:a00", Key::Return),
    ] {
        tab_to(&app, to);
        key(&app, press);
    }
    assert_eq!(
        *calls.borrow(),
        [
            "close",
            "reveal S2",
            "open",
            "filter Done",
            "select a00 false",
            "inspect a00"
        ]
    );
    tab_to(&app, "Extract");
    key(&app, Key::Return);
    assert_eq!(app.get_mode(), Mode::Extract);
    app.hide().unwrap();
}

/// A control focused from the keyboard draws its ring; one pressed with the pointer does not,
/// until a key is typed (a modifier alone does not count), as `:focus-visible` does.
#[test]
fn the_ring_shows_for_keyboard_focus_not_after_a_pointer_press() {
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    SlintPublisher::new(&app).show(&three_releases());
    app.show().unwrap();
    assert!(rings(&app).is_empty(), "no ring at start");

    tab_to(&app, "Toggle theme");
    let theme = only(&app, "Toggle theme");
    let (at, size) = (theme.absolute_position(), theme.size());
    assert_eq!(
        rings(&app),
        [(at.x - 4.0, at.y - 4.0, size.width + 8.0, size.height + 8.0)],
        "2px out from the button, 2px wide"
    );

    // Both panes' chips are found, the extract one hidden in the same place.
    let chip = ElementHandle::find_by_accessible_label(&app, "All 0")
        .next()
        .unwrap();
    pointer(&app, &chip, true);
    pointer(&app, &chip, false);
    assert_eq!(focused(&app), "remux:All", "a press takes the focus");
    assert!(rings(&app).is_empty(), "with no ring");
    key(&app, Key::Shift);
    assert!(rings(&app).is_empty(), "a modifier alone shows none");
    key(&app, "x");
    assert_eq!(rings(&app).len(), 1, "a typed key shows it");
    app.hide().unwrap();
}

/// A dialog and the sheet give the focus back to the exact control that had it, the ring only
/// after a key closed them; the sheet's field takes it back after a dialog over the sheet. A
/// control locked meanwhile leaves it with the window, where Esc still closes the Log drawer.
#[test]
fn a_closing_dialog_or_sheet_gives_the_focus_back_to_the_control_that_had_it() {
    use mkv_anime_batch_slint_ui::DialogTexts;
    use slint::platform::Key;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    SlintPublisher::new(&app).show(&three_releases());
    app.show().unwrap();
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    let notice = DialogTexts {
        title: "Notice".into(),
        text: "Something happened".into(),
        confirm: "OK".into(),
        notice: true,
    };
    let show_notice = || {
        app.set_dialog(notice.clone());
        app.invoke_show_dialog();
        tick();
    };
    let close_dialog = || {
        app.set_dialog_open(false);
        tick();
        tick();
    };

    // A dialog over the window: the Close button has it back.
    tab_to(&app, "Close window");
    show_notice();
    assert_eq!(focused(&app), "OK", "OK takes it as the dialog shows");
    close_dialog();
    assert_eq!(focused(&app), "Close window");
    assert_eq!(rings(&app).len(), 1);

    // The sheet from a gear, closed by Esc: the gear, ringed.
    tab_to(&app, "Remux settings");
    key(&app, Key::Return);
    tick();
    assert!(app.get_sheet_open());
    assert_eq!(focused(&app), "Done");
    key(&app, Key::Escape);
    tick();
    assert!(!app.get_sheet_open());
    assert_eq!(focused(&app), "Remux settings");
    assert_eq!(rings(&app).len(), 1);
    // Closed by a click on the backdrop: the gear, without a ring.
    key(&app, Key::Return);
    tick();
    assert!(app.get_sheet_open());
    for pressed in [true, false] {
        use slint::platform::{PointerEventButton, WindowEvent};
        let (position, button) = (
            slint::LogicalPosition::new(20.0, 625.0),
            PointerEventButton::Left,
        );
        app.window().dispatch_event(if pressed {
            WindowEvent::PointerPressed { position, button }
        } else {
            WindowEvent::PointerReleased { position, button }
        });
    }
    tick();
    assert!(!app.get_sheet_open());
    assert_eq!(focused(&app), "Remux settings");
    assert!(rings(&app).is_empty(), "a pointer closed it");

    // The sheet the host opens: the control that had the focus.
    tab_to(&app, "remux:None");
    app.invoke_open_sheet(Mode::Remux, "".into());
    tick();
    key(&app, Key::Escape);
    tick();
    assert_eq!(focused(&app), "remux:None");

    // A dialog over the sheet's field: the field has the cursor back.
    app.invoke_open_sheet(Mode::Remux, "input_dir".into());
    tick();
    tick();
    let sheet = app.global::<Sheet>();
    assert_eq!(sheet.get_focused(), "input_dir");
    show_notice();
    assert_eq!(sheet.get_focused(), "", "the dialog took the focus");
    close_dialog();
    assert_eq!(sheet.get_focused(), "input_dir");
    key(&app, Key::Escape);
    tick();
    assert!(!app.get_sheet_open());

    // Select all, locked while the dialog shows: the window keeps the focus, Esc still works.
    tab_to(&app, "remux:Select all");
    show_notice();
    app.set_remux_head(mkv_anime_batch_slint_ui::QueueHead {
        selection_locked: true,
        ..app.get_remux_head()
    });
    app.set_log_open(true);
    close_dialog();
    assert_eq!(focused(&app), "", "no control took it");
    key(&app, Key::Escape);
    assert!(!app.get_log_open(), "Esc closed the drawer");
    app.hide().unwrap();
}

/// A notice's hidden Cancel takes no focus: neither a restore nor Tab reaches it.
#[test]
fn a_notices_hidden_cancel_takes_no_focus() {
    use mkv_anime_batch_slint_ui::DialogTexts;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    SlintPublisher::new(&app).show(&three_releases());
    app.show().unwrap();
    let tick = || i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(1));
    app.set_dialog(DialogTexts {
        title: "Notice".into(),
        text: "Something happened".into(),
        confirm: "OK".into(),
        notice: true,
    });
    app.invoke_show_dialog();
    tick();
    assert_eq!(focused(&app), "OK");
    app.global::<Focus>().invoke_restore("Cancel".into());
    tick();
    assert_eq!(focused(&app), "OK", "a restore passes it over");
    for _ in 0..4 {
        key(&app, slint::platform::Key::Tab);
        assert_ne!(focused(&app), "Cancel", "Tab passes it over");
    }
    app.hide().unwrap();
}

/// The pinned release header: none at the top of the list, the folder's once its rows scroll
/// under it, none while a release's first row shows its own header whole at the top, that
/// release's once it scrolls on, and still that release's at the end of the list.
#[test]
fn the_current_release_header_stays_pinned_at_the_top_of_the_list() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let publisher = SlintPublisher::new(&app);
    publisher.show(&three_releases());
    app.show().unwrap();
    frame(&app);
    let pinned = |app: &App| {
        let text = app.get_remux_pinned();
        (!text.is_empty()).then(|| format!("{text} {}", app.get_remux_pinned_count()))
    };
    let top = only(&app, "Include a00 in the batch").absolute_position().y - 30.0 - 18.0;

    assert_eq!(pinned(&app), None, "the first header shows in place");
    app.set_remux_scroll(-500.0);
    frame(&app);
    assert_eq!(pinned(&app).as_deref(), Some("MEDIA › SHOW 20"));
    // The copy over the rows, which no screen reader reads (the row's own header is read).
    let head = ElementHandle::find_by_element_type_name(&app, "GroupHead")
        .find(|head| head.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::None))
        .unwrap();
    assert_eq!(head.absolute_position().y, top, "at the list's top");

    // A filter that hides the pinned release's rows unpins its header, and the count with it.
    publisher.show_filter(Mode::Remux, QueueFilter::Done);
    frame(&app);
    assert_eq!(pinned(&app), None, "no row shows: no header pinned");
    assert_eq!(app.get_remux_pinned_count(), 0);
    publisher.show_filter(Mode::Remux, QueueFilter::All);
    app.set_remux_scroll(-500.0);
    frame(&app);
    assert_eq!(pinned(&app).as_deref(), Some("MEDIA › SHOW 20"));

    // The same number of rows under other release heads (the folder renamed): the row instances
    // stay, and the header pinned is the new one, never the old.
    let mut renamed = three_releases();
    renamed.remux.library = presentation::library(Some("/media/Other"), &renamed.remux.rows);
    renamed.remux.manifest_rev = 2;
    renamed.remux.directory = Some("/media/Other".into());
    // A later state: the publisher drops one no newer than the last.
    renamed.seq = 1;
    publisher.show(&renamed);
    frame(&app);
    app.set_remux_scroll(-500.0);
    frame(&app);
    assert_eq!(pinned(&app).as_deref(), Some("MEDIA › OTHER 20"));
    publisher.show(&ViewState {
        seq: 2,
        ..three_releases()
    });
    frame(&app);

    app.invoke_reveal(Mode::Remux, 20);
    frame(&app);
    assert_eq!(pinned(&app), None, "S2's own header shows whole at the top");
    app.set_remux_scroll(app.get_remux_scroll() - 40.0);
    frame(&app);
    assert_eq!(pinned(&app).as_deref(), Some("S2 20"));

    app.set_remux_scroll(-100_000.0);
    frame(&app);
    assert_eq!(
        pinned(&app).as_deref(),
        Some("S2 20"),
        "S3's four rows fit below S2's last"
    );
    // S3 revealed again from there: the list stays at its end, and so does the header.
    for _ in 0..2 {
        app.invoke_reveal(Mode::Remux, 40);
        frame(&app);
        assert_eq!(pinned(&app).as_deref(), Some("S2 20"), "a repeat reveal");
    }

    // Another folder, of one release (no headers), replaces every row instance: the old
    // folder's header must not stay pinned over its rows.
    let mut one = three_releases();
    let rows: Vec<Row> = (0..44).map(|i| row(&format!("d{i:02}"))).collect();
    one.remux.library = presentation::library(Some("/media/Other"), &rows);
    one.remux.rows = Arc::new(rows);
    one.remux.manifest_rev = 2;
    one.remux.directory = Some("/media/Other".into());
    SlintPublisher::new(&app).show(&one);
    frame(&app);
    app.set_remux_scroll(-500.0);
    frame(&app);
    assert_eq!(pinned(&app), None, "no header pinned from the old folder");
    app.hide().unwrap();
}

/// The subtitle ladder is a list of list items, each numbered, as index.html's `ol`.
#[test]
fn the_subtitle_ladder_is_a_list_of_its_rungs() {
    use i_slint_backend_testing::AccessibleRole;
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 900.0));
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![row("Show - 01.mkv")]);
    state.remux.empty = None;
    state.remux.inspector = Some(sample_view());
    apply(&app, &state);
    app.show().unwrap();
    let rungs: Vec<_> = app.get_sub_ladder().iter().collect();
    assert!(rungs.len() > 1, "the sample has a ladder");
    // An element is found once per component it is the root of: one of each place.
    let query = |role| {
        let mut found: Vec<ElementHandle> = Vec::new();
        for element in i_slint_backend_testing::ElementQuery::from_root(&app)
            .match_descendants()
            .match_accessible_role(role)
            .find_all()
        {
            let place = |element: &ElementHandle| (element.absolute_position(), element.size());
            if !found.iter().any(|seen| place(seen) == place(&element)) {
                found.push(element);
            }
        }
        found
    };
    let ladder: Vec<_> = query(AccessibleRole::List)
        .into_iter()
        .filter(|list| list.accessible_item_count() == Some(rungs.len()))
        .collect();
    assert_eq!(ladder.len(), 1, "one list of the rungs");
    let items: Vec<_> = query(AccessibleRole::ListItem)
        .iter()
        .map(|item| {
            (
                item.accessible_item_index(),
                item.accessible_label().unwrap().to_string(),
            )
        })
        .collect();
    let expected: Vec<_> = rungs
        .iter()
        .enumerate()
        .map(|(index, rung)| (Some(index), rung.a11y.to_string()))
        .collect();
    assert_eq!(items, expected);
    app.hide().unwrap();
}

/// Move down from the keyboard: the reorder resets the rows, and the moved episode's Move down
/// takes the focus back as its new row is made, step after step. A row a rescan's reset drops
/// still hands the focus to the window's keys.
#[test]
fn a_reorder_keeps_the_focus_on_the_moved_episodes_button() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut state = ViewState::new(None);
    let rows = vec![row("a"), row("b"), row("c")];
    state.remux.rows = Arc::new(rows.clone());
    state.remux.empty = None;
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>().invoke_restore("down:remux:a".into());
    frame(&app);
    assert_eq!(focused(&app), "down:remux:a");

    for (seq, order) in [(1, [1, 0, 2]), (2, [1, 2, 0])] {
        let mut moved = state.clone();
        moved.seq = seq;
        moved.remux.rows = Arc::new(order.map(|i| rows[i].clone()).into());
        publisher.show(&moved);
        frame(&app);
        assert_eq!(focused(&app), "down:remux:a", "after step {seq}");
    }

    // A rescan without the episode: the window's keys have the focus.
    let mut rescan = state.clone();
    rescan.seq = 3;
    rescan.remux.rows = Arc::new(vec![row("b"), row("c")]);
    publisher.show(&rescan);
    frame(&app);
    assert_eq!(focused(&app), "");
    assert_window_keys(&app);
    app.hide().unwrap();
}

/// A reorder that moves the episode out of view leaves the focus on the window's keys, and a
/// later scroll that makes its row gives its Move down no focus.
#[test]
fn a_reorder_out_of_view_gives_a_row_made_later_no_focus() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(920.0, 640.0));
    let mut state = ViewState::new(None);
    let rows: Vec<Row> = (0..30).map(|i| row(&format!("r{i:02}"))).collect();
    state.remux.rows = Arc::new(rows.clone());
    state.remux.empty = None;
    let publisher = SlintPublisher::new(&app);
    publisher.show(&state);
    app.show().unwrap();
    frame(&app);
    app.global::<Focus>()
        .invoke_restore("down:remux:r00".into());
    frame(&app);
    assert_eq!(focused(&app), "down:remux:r00");

    let mut moved = state.clone();
    moved.seq = 1;
    let mut order = rows[1..].to_vec();
    order.push(rows[0].clone());
    moved.remux.rows = Arc::new(order);
    publisher.show(&moved);
    frame(&app);
    assert_eq!(focused(&app), "", "the moved row is out of view");

    app.invoke_reveal(Mode::Remux, 29);
    frame(&app);
    assert_eq!(
        focused(&app),
        "",
        "the row made by the scroll takes no focus"
    );
    assert_window_keys(&app);
    app.hide().unwrap();
}

/// Reduce motion stops the spinning rings: a processing row's ring, drawn, turns with the
/// animation tick, and stands still under Reduce motion, the window drawing no frame for it.
#[test]
fn reduce_motion_stops_the_spinning_rings() {
    use std::time::Duration;
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .unwrap();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    let mut state = ViewState::new(None);
    state.remux.rows = Arc::new(vec![Row {
        status: ItemStatus::Processing,
        label: "Processing".into(),
        tone: Tone::Info,
        ..row("Show - 01.mkv")
    }]);
    state.remux.empty = None;
    apply(&app, &state);
    app.show().unwrap();
    // A frame drawn once the effects settled: does it read the animation tick? The software
    // renderer does not rotate, so the turn itself cannot be seen in the pixels.
    let animated = || {
        i_slint_backend_testing::mock_elapsed_time(Duration::from_millis(1000));
        app.window().take_snapshot().unwrap();
        app.window().has_active_animations()
    };
    assert!(animated(), "the ring spins");
    app.global::<Theme>().set_reduce_motion(true);
    assert!(!animated(), "the ring stands still");
    app.hide().unwrap();
}

/// Ctrl+T held flips the theme once, and Ctrl+Shift+T (an upper-case "T") flips it too.
#[test]
fn ctrl_t_ignores_its_repeats_and_takes_an_upper_case_t() {
    use slint::platform::{Key, WindowEvent};
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let flips = Rc::new(Cell::new(0));
    let count = Rc::clone(&flips);
    app.global::<Theme>()
        .on_toggle(move || count.set(count.get() + 1));
    let send = |event| app.window().dispatch_event(event);
    send(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    send(WindowEvent::KeyPressed { text: "t".into() });
    for _ in 0..3 {
        send(WindowEvent::KeyPressRepeated { text: "t".into() });
    }
    send(WindowEvent::KeyReleased { text: "t".into() });
    assert_eq!(flips.get(), 1, "one flip for a held Ctrl+T");
    key(&app, "T");
    assert_eq!(flips.get(), 2, "an upper-case T flips it");
    send(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
    app.hide().unwrap();
}

/// The Appearance button reports its menu open or closed, and a screen reader's expand opens
/// it, as the Log toggle's does.
#[test]
fn the_appearance_button_reports_its_menu_expanded() {
    i_slint_backend_testing::init_no_event_loop();
    let app = new_app();
    app.window().set_size(slint::LogicalSize::new(760.0, 520.0));
    apply(&app, &ViewState::new(None));
    app.show().unwrap();
    let button = appearance_button(&app);
    assert_eq!(button.accessible_expandable(), Some(true));
    assert_eq!(button.accessible_expanded(), Some(false));
    button.invoke_accessible_expand_action();
    assert!(app.get_appearance_open());
    assert_eq!(button.accessible_expanded(), Some(true));
    assert_eq!(focused(&app), "Appearance");
    app.hide().unwrap();
}
