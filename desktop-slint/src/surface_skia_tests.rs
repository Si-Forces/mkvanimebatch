//! Real-window Skia OpenGL regression test for #254 (filter chips overlap after `set_surface`
//! clears the text layout cache without dirtying dependents).
//!
//! Lives in the binary crate so it exercises `main::watch_start`'s real `Surface.epoch` bump
//! instead of bumping from a test-only notifier. Ignored in CI (needs a display and OpenGL).
//! Run alone so winit + skia-opengl does not clash with `ui_tests`' testing backend:
//! `cargo test -p mkv-anime-batch-slint surface_skia -- --ignored --test-threads=1 --nocapture`

use crate::renderer::GpuStart;
use crate::watch_start;
use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::items::{ComplexText, ItemRc, Orientation, SimpleText};
use i_slint_core::window::WindowInner;
use mkv_anime_batch_slint_ui::{App, QueueFilter, QueueHead, Theme};
use slint::{ComponentHandle, LogicalSize, SharedString, Timer, TimerMode};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// `.list-tools` `FlexboxLayout` spacing in `desktop-slint/ui/queue.slint`.
const CHIP_SPACING: f32 = 6.0;
/// `Chip` inner horizontal spacing (4px) + horizontal padding (24px) in `queue.slint`.
const CHIP_CHROME_W: f32 = 28.0;
const TOLERANCE: f32 = 1.0;
/// The >=1180px layout (220px sidebar, 320px Inspector). Needs a work area of at least 1200x660
/// logical px (1500x825 physical at 125%): a window manager that clamps it makes step 3 time out.
const WIDE_SIZE: LogicalSize = LogicalSize {
    width: 1200.0,
    height: 660.0,
};
const NARROW_SIZE: LogicalSize = LogicalSize {
    width: 760.0,
    height: 520.0,
};

#[derive(Debug, Clone)]
struct ChipGeom {
    label: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    expected_w: f32,
}

fn window_matches_size(app: &App, want: LogicalSize) -> bool {
    let scale = app.window().scale_factor();
    let got = app.window().size().to_logical(scale);
    (got.width - want.width).abs() <= TOLERANCE && (got.height - want.height).abs() <= TOLERANCE
}

/// Finds the single visible filter chip with `label`. The hidden extract pane has its own
/// chips with the same labels; they are excluded because `visible: false` clips them
/// (`ItemRc::is_visible`), and the "exactly one" check fails loudly if that ever changes.
fn find_visible_chip(root: &ItemRc, label: &str) -> Result<ItemRc, String> {
    let mut matches = Vec::new();
    fn walk(item: ItemRc, label: &str, matches: &mut Vec<ItemRc>) {
        if item
            .accessible_string_property(AccessibleStringProperty::Label)
            .as_deref()
            == Some(label)
            && item.is_visible()
        {
            let geom = item.geometry();
            if geom.width() > 0.0 && geom.height() > 0.0 {
                matches.push(item.clone());
            }
        }
        let mut child = item.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            walk(c, label, matches);
            child = next;
        }
    }
    walk(root.clone(), label, &mut matches);
    match matches.len() {
        1 => Ok(matches.pop().unwrap()),
        n => Err(format!(
            "expected 1 visible chip labelled {label:?}, found {n}"
        )),
    }
}

/// Walks only `chip`'s subtree (rather than rect containment, which rejects a stale chip's
/// overflowing text) and requires exactly one visible matching `Text`.
fn live_text_preferred_width(
    chip: &ItemRc,
    chip_label: &str,
    text: &str,
    adapter: &Rc<dyn i_slint_core::window::WindowAdapter>,
) -> Result<f32, String> {
    let mut matches = Vec::new();
    fn walk(
        item: ItemRc,
        needle: &str,
        adapter: &Rc<dyn i_slint_core::window::WindowAdapter>,
        matches: &mut Vec<f32>,
    ) {
        let content = item
            .downcast::<SimpleText>()
            .map(|t| t.as_pin_ref().text())
            .or_else(|| {
                item.downcast::<ComplexText>()
                    .map(|t| t.as_pin_ref().text())
            });
        if content.as_deref() == Some(needle) && item.is_visible() {
            let geom = item.geometry();
            if geom.width() > 0.0 && geom.height() > 0.0 {
                let info = item.borrow().as_ref().layout_info(
                    Orientation::Horizontal,
                    -1.0,
                    adapter,
                    &item,
                );
                matches.push(info.preferred);
            }
        }
        let mut child = item.first_child();
        while let Some(c) = child {
            let next = c.next_sibling();
            walk(c, needle, adapter, matches);
            child = next;
        }
    }
    walk(chip.clone(), text, adapter, &mut matches);
    match matches.as_slice() {
        [w] => Ok(*w),
        _ => Err(format!(
            "expected 1 visible Text {text:?} inside chip {chip_label:?}, found {}",
            matches.len()
        )),
    }
}

fn read_chips(app: &App) -> Result<Vec<ChipGeom>, String> {
    let win_inner = WindowInner::from_pub(app.window());
    let adapter = win_inner.window_adapter();
    let root = ItemRc::new_root(win_inner.component());
    let specs = [
        ("All", "1000"),
        ("To do", "875"),
        ("Done", "125"),
        ("Failed", "0"),
    ];
    let mut out = Vec::with_capacity(specs.len());
    for (name, count) in specs {
        let label = format!("{name} {count}");
        let chip = find_visible_chip(&root, &label)?;
        let geom = chip.geometry();
        let pos = chip.map_to_window(geom.origin);
        let expected_w = live_text_preferred_width(&chip, &label, name, &adapter)?
            + live_text_preferred_width(&chip, &label, count, &adapter)?
            + CHIP_CHROME_W;
        out.push(ChipGeom {
            label,
            x: pos.x,
            y: pos.y,
            w: geom.width(),
            h: geom.height(),
            expected_w,
        });
    }
    Ok(out)
}

fn check_chips(scene: &str, chips: &[ChipGeom], single_row: bool) -> Result<(), String> {
    println!("[{scene}] chips:");
    for c in chips {
        println!(
            "  {:<10} x={:6.1} y={:6.1} w={:5.1} (expected {:5.1}) h={:4.1} right={:6.1}",
            c.label,
            c.x,
            c.y,
            c.w,
            c.expected_w,
            c.h,
            c.x + c.w
        );
    }
    if single_row {
        let y0 = chips[0].y;
        for c in &chips[1..] {
            if (c.y - y0).abs() > TOLERANCE {
                return Err(format!(
                    "[{scene}] expected single row, but {} y={:.1} != {} y={:.1}",
                    c.label, c.y, chips[0].label, y0
                ));
            }
        }
    } else if chips.iter().all(|c| (c.y - chips[0].y).abs() <= TOLERANCE) {
        return Err(format!(
            "[{scene}] expected chips to wrap at narrow width, but all sit at y={:.1}",
            chips[0].y
        ));
    }
    for i in 0..chips.len() {
        for j in (i + 1)..chips.len() {
            let (a, b) = (&chips[i], &chips[j]);
            let overlap_x = a.x < b.x + b.w - 0.5 && b.x < a.x + a.w - 0.5;
            let overlap_y = a.y < b.y + b.h - 0.5 && b.y < a.y + a.h - 0.5;
            if overlap_x && overlap_y {
                return Err(format!(
                    "[{scene}] overlap between {} ({:.1}..{:.1}, y={:.1}) and {} ({:.1}..{:.1}, y={:.1})",
                    a.label,
                    a.x,
                    a.x + a.w,
                    a.y,
                    b.label,
                    b.x,
                    b.x + b.w,
                    b.y
                ));
            }
        }
    }
    for pair in chips.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if (a.y - b.y).abs() <= TOLERANCE {
            let gap = b.x - (a.x + a.w);
            if (gap - CHIP_SPACING).abs() > TOLERANCE {
                return Err(format!(
                    "[{scene}] gap between {} and {} is {gap:.1}px, expected {CHIP_SPACING}px",
                    a.label, b.label
                ));
            }
        }
    }
    for c in chips {
        if (c.w - c.expected_w).abs() > TOLERANCE {
            return Err(format!(
                "[{scene}] chip {:?} width {:.1}px != live preferred {:.1}px (stale pre-show size)",
                c.label, c.w, c.expected_w
            ));
        }
    }
    Ok(())
}

fn record_err(slot: &RefCell<Option<String>>, result: Result<(), String>) {
    let Err(err) = result else { return };
    if slot.borrow().is_none() {
        *slot.borrow_mut() = Some(err);
    }
}

#[test]
#[ignore = "requires a display, Skia OpenGL and a 1200x660 logical work area; run alone with `cargo test -p mkv-anime-batch-slint surface_skia -- --ignored --test-threads=1`"]
fn filter_chips_resize_and_do_not_overlap_on_skia() {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia-opengl".into())
        .select()
        .expect("select winit + skia-opengl");

    let app = App::new().expect("App::new");
    // Pin light explicitly so boot-light / wide-light do not inherit a dark desktop scheme.
    app.global::<Theme>().invoke_set_dark(false);
    let start = Rc::new(Cell::new(GpuStart::Setup));
    watch_start(&app, None, Rc::clone(&start));

    let head = QueueHead {
        title: SharedString::from("Initial"),
        ..Default::default()
    };
    app.set_remux_head(head);

    let error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let step = Rc::new(Cell::new(0u8));
    let ticks = Rc::new(Cell::new(0u32));

    let driver = Timer::default();
    let weak = app.as_weak();
    let start_cb = Rc::clone(&start);
    let error_cb = Rc::clone(&error);
    let step_cb = Rc::clone(&step);
    let ticks_cb = Rc::clone(&ticks);

    // One repeated timer drives the scenes without replacing `watch_start`'s rendering notifier
    // and without manual `request_redraw()` calls, exercising the production repaint path.
    driver.start(TimerMode::Repeated, Duration::from_millis(150), move || {
        ticks_cb.set(ticks_cb.get() + 1);
        if ticks_cb.get() > 60 {
            record_err(
                &error_cb,
                Err(format!(
                    "timed out waiting for Skia frames at step {}",
                    step_cb.get()
                )),
            );
            let _ = slint::quit_event_loop();
            return;
        }
        let Some(app) = weak.upgrade() else { return };

        match step_cb.get() {
            // Wait until `watch_start` observes the first `AfterRendering` (and bumps `Surface.epoch`).
            0 => {
                if start_cb.get() == GpuStart::Drew {
                    step_cb.set(1);
                }
            }
            // One tick after the first frame: mutate counts at boot size (no resize, no request_redraw).
            1 => {
                let mut head = app.get_remux_head();
                head.all = 1000;
                head.todo = 875;
                head.done = 125;
                head.failed = 0;
                head.filter = QueueFilter::Failed;
                app.set_remux_head(head);
                step_cb.set(2);
            }
            2 => {
                record_err(
                    &error_cb,
                    read_chips(&app).and_then(|c| check_chips("boot-light", &c, true)),
                );
                app.window().set_size(WIDE_SIZE);
                step_cb.set(3);
            }
            // On Wayland (`request_inner_size` returns `Some` and runs `resize_event` inside
            // `set_size`) the size already matches on the next tick; on X11/Windows the `Resized`
            // event arrives later, which is what this wait is for. Advance to check on the next tick.
            3 => {
                if window_matches_size(&app, WIDE_SIZE) {
                    step_cb.set(4);
                }
            }
            4 => {
                record_err(
                    &error_cb,
                    read_chips(&app).and_then(|c| check_chips("wide-light", &c, true)),
                );
                app.global::<Theme>().invoke_set_dark(true);
                step_cb.set(5);
            }
            5 => {
                record_err(
                    &error_cb,
                    read_chips(&app).and_then(|c| check_chips("wide-dark", &c, true)),
                );
                app.window().set_size(NARROW_SIZE);
                step_cb.set(6);
            }
            // Same as step 3: on Wayland the size already matches on the next tick; on X11/Windows
            // this waits for the later `Resized` event before advancing to check on the next tick.
            6 => {
                if window_matches_size(&app, NARROW_SIZE) {
                    step_cb.set(7);
                }
            }
            7 => {
                record_err(
                    &error_cb,
                    read_chips(&app).and_then(|c| check_chips("narrow-dark", &c, false)),
                );
                app.global::<Theme>().invoke_set_dark(false);
                step_cb.set(8);
            }
            8 => {
                record_err(
                    &error_cb,
                    read_chips(&app).and_then(|c| check_chips("narrow-light", &c, false)),
                );
                let _ = slint::quit_event_loop();
                step_cb.set(9);
            }
            _ => {}
        }
    });

    if start.get() == GpuStart::Setup {
        start.set(GpuStart::Pending);
    }
    app.run().expect("app.run");

    let msg = error.borrow().clone();
    if let Some(msg) = msg {
        panic!("{msg}");
    }
    assert_eq!(
        step.get(),
        9,
        "driver did not finish all scenes (ended at step {})",
        step.get()
    );
}
