//! The queue's FrameStats bench (#135; P7 and the P9 gate reuse it), ported from the #102 spike.
//! 1000 synthetic rows (some with CJK names) in the real P2 queue view; a batch publishes at about
//! 15 Hz through the real Publisher path (rows mapped and diffed off the UI thread, then the hop's
//! `set_row_data`); the list scrolls 6 px per frame for 30 s after a 3 s warm-up, on Skia OpenGL,
//! maximized, without the glass. Prints one `BENCH` line and exits.
//!
//! ```text
//! cargo bench -p mkv-anime-batch-slint --bench queue_scroll
//! env -u WAYLAND_DISPLAY cargo bench -p mkv-anime-batch-slint --bench queue_scroll   # Xwayland
//! ```
//!
//! Each `cargo bench` builds the UI crate again (slint-build watches a `slint-project.json` that
//! does not exist), with the release profile's LTO; to repeat a run, start the executable it
//! printed (`target/release/deps/queue_scroll-*`) directly.
//!
//! `map_ms` is each publication's push on the publishing thread, off the UI thread: the rows
//! mapped, then the diff against the model under the coalescer's lock. `pub_ms` is the UI-thread
//! time of each publication's hop (the take and the model writes), not the layout those writes
//! cause, which the next frame's `render_ms` carries; `reset_ms` the first hop, which puts all
//! 1000 rows into the empty model; `filter_ms` the hop of a filter change to Failed at the end,
//! which writes nearly every row (the bench maps it on the UI thread before the timed hop; the app
//! maps it off the UI thread). The Library is the core's for these rows, so each release's group
//! header is its own. `interval_ms` is the time between presented frames (vsync-bound, so a hair
//! over 16.67 ms is jitter, and over 25 ms a missed vsync); `render_ms` each frame's rendering on
//! the UI thread.
//! Halfway through the window a Run's forced scan publishes a new `manifest_rev` with the same
//! rows, which resets the whole row model (`queue::diff_rows`): `run_reset_ms` is that hop's
//! UI-thread time, and `run_reset_frame` the render and interval of the frame it lands in (the
//! first presented after it). The churn's clock and progress stack redraw the bottom bar (P5).
//! Budget (#135): publication p99 < 4 ms, and no frame over 16.7 ms, read as no render over
//! 16.7 ms and no missed vsync. The line ends with PASS or FAIL for each.

// Only the hop's path is driven here; the rest of these host modules is unused. `clippy
// --all-targets` checks a bench with cfg(test) but without its #[test] items, which leaves their
// imports unused.
#[allow(dead_code, unused_imports)]
#[path = "../src/inspector.rs"]
mod inspector;
#[allow(dead_code, unused_imports)]
#[path = "../src/intents.rs"]
mod intents;
// The hop writes the job controls too (P5).
#[allow(dead_code, unused_imports)]
#[path = "../src/jobs.rs"]
mod jobs;
#[allow(dead_code, unused_imports)]
#[path = "../src/publisher.rs"]
mod publisher;
#[allow(dead_code, unused_imports)]
#[path = "../src/queue.rs"]
mod queue;
// The hop writes the Run Log consoles too (P5).
#[allow(dead_code, unused_imports)]
#[path = "../src/run_log.rs"]
mod run_log;
// The hop writes the drafts, chips and presets too (P4).
#[allow(dead_code, unused_imports)]
#[path = "../src/settings.rs"]
mod settings;
// The hop writes the telemetry too (P6).
#[allow(dead_code, unused_imports)]
#[path = "../src/telemetry.rs"]
mod telemetry;

use mkv_anime_batch_slint_ui::{App, Mode, QueueFilter, Theme};
use mkv_desktop_core::actor::ViewState;
use mkv_desktop_core::presentation::{
    bucket, library, status_label, status_title, status_tone, Tone,
};
use mkv_desktop_core::queue::Row;
use mkv_desktop_core::snapshot::ItemStatus;
use publisher::Coalescer;
use slint::{ComponentHandle, RenderingState};
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: usize = 1000;
/// Pixels the list scrolls per frame.
const STEP: f32 = 6.0;
const WARMUP: Duration = Duration::from_secs(3);
const WINDOW: Duration = Duration::from_secs(30);
/// The core's publication cap (`PUBLISH_INTERVAL`, about 15 Hz).
const PUBLISH_INTERVAL: Duration = Duration::from_millis(66);
/// How many rows the batch processes at once.
const IN_FLIGHT: usize = 8;
/// A row's and a release header's height in queue.slint.
const ROW_H: f32 = 52.0;
const HEAD_H: f32 = 30.0;

const SHOWS: &[(&str, &str)] = &[
    ("SubsPlease", "Frieren - Beyond Journey's End"),
    ("Nekomoe kissaten", "葬送のフリーレン"),
    // Third, so the 30 s scroll reaches its Simplified Chinese names after the Japanese ones.
    ("LoliHouse", "药屋少女的呢喃"),
    ("Judas", "Mushoku Tensei - Jobless Reincarnation"),
    ("EMBER", "Dungeon Meshi"),
    ("Sakurato", "ダンジョン飯"),
    ("SubsPlease", "Spy x Family"),
    ("Yameii", "【推しの子】"),
    ("Tsundere-Raws", "Vinland Saga"),
    ("DKB", "Jujutsu Kaisen"),
];

/// `n` rows in releases of 100 episodes: the first tenth done, a few failed or skipped, the rest
/// queued, with every hundred-and-first left out of the selection.
fn synthetic_rows(n: usize) -> Vec<Row> {
    (0..n)
        .map(|i| {
            let (group, show) = SHOWS[(i / 100) % SHOWS.len()];
            let release = format!("[{group}] {show} (1080p)");
            let hash = (i as u32).wrapping_mul(2_654_435_761) ^ 0x5bd1_e995;
            let name = format!(
                "[{group}] {show} - {:02} (1080p) [{hash:08X}].mkv",
                i % 100 + 1
            );
            let status = match i {
                _ if i < n / 10 => ItemStatus::Completed,
                _ if i % 97 == 13 => ItemStatus::Error,
                _ if i % 53 == 7 => ItemStatus::Skipped,
                _ => ItemStatus::Queued,
            };
            let mut row = Row {
                id: format!("{release}/{name}"),
                name,
                release,
                in_manifest: true,
                selected: i % 101 != 50,
                status,
                progress: None,
                participant: true,
                label: String::new(),
                tone: Tone::Mute,
                title: String::new(),
                meta: match status {
                    ItemStatus::Error => "mkvmerge exited with code 2: unsupported codec".into(),
                    ItemStatus::Completed => {
                        "Opus 128k · subs: same-directory external (ind)".into()
                    }
                    _ => "FLAC → Opus 128k · subs: embedded (eng) · 3 fonts".into(),
                },
                meta_error: status == ItemStatus::Error,
                bucket: None,
            };
            refresh(&mut row, Duration::ZERO, Some(3.0 + (i % 40) as f64 * 0.7));
            row
        })
        .collect()
}

/// Recomputes the presented fields of `row` the way the core does.
fn refresh(row: &mut Row, elapsed: Duration, duration_sec: Option<f64>) {
    row.label = status_label(row.status, row.progress, Some(elapsed), duration_sec);
    row.tone = status_tone(row.status);
    row.title = status_title(row.status, None, None);
    row.bucket = bucket(row.status, row.selected);
}

/// Starts a thread that advances `IN_FLIGHT` processing rows and publishes `state` with them every
/// `PUBLISH_INTERVAL`, like a remux run seen through the core. Labels tick once a second; when a
/// row finishes the next queued one starts, and a finished batch starts again.
/// Once `reset` is set, the next publication made while no hop is in flight (`in_flight` is 0)
/// bumps `manifest_rev`, so the hop it schedules is the one that applies the reset.
fn spawn_churn(
    mut state: ViewState,
    reset: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
    mut publish: impl FnMut(&ViewState, bool) + Send + 'static,
) {
    std::thread::spawn(move || {
        let mut rows = (*state.remux.rows).clone();
        let mut started: Vec<(usize, Instant)> = Vec::new();
        let mut next = 0;
        let t0 = Instant::now();
        let mut last_second = 0;
        loop {
            while started.len() < IN_FLIGHT {
                while next < rows.len()
                    && !(rows[next].selected && rows[next].status == ItemStatus::Queued)
                {
                    next += 1;
                }
                if next == rows.len() {
                    break;
                }
                rows[next].status = ItemStatus::Processing;
                rows[next].progress = Some(0.0);
                refresh(&mut rows[next], Duration::ZERO, None);
                started.push((next, Instant::now()));
                next += 1;
            }
            let second = t0.elapsed().as_secs();
            let tick = std::mem::replace(&mut last_second, second) != second;
            let mut finished = Vec::new();
            for (slot, &(i, at)) in started.iter().enumerate() {
                let row = &mut rows[i];
                // Each slot runs at its own speed: 6 to 13 s per item.
                let speed = 100.0 / (6.0 + slot as f64) * PUBLISH_INTERVAL.as_secs_f64();
                let before = row.progress.map(f64::round);
                let p = (row.progress.unwrap_or(0.0) + speed).min(100.0);
                row.progress = Some(p);
                if p >= 100.0 {
                    row.status = ItemStatus::Completed;
                    row.progress = None;
                    refresh(row, at.elapsed(), Some(at.elapsed().as_secs_f64()));
                    row.meta = "Opus 128k · subs: same-directory external (ind)".into();
                    finished.push(slot);
                } else if tick || before != Some(p.round()) {
                    refresh(row, at.elapsed(), None);
                }
            }
            for slot in finished.into_iter().rev() {
                started.swap_remove(slot);
            }
            if started.is_empty() {
                let done_at_start = rows.len() / 10;
                for row in rows.iter_mut().skip(done_at_start) {
                    if row.status == ItemStatus::Completed {
                        row.status = ItemStatus::Queued;
                        refresh(row, Duration::ZERO, None);
                    }
                }
                next = 0;
            }
            let done = rows
                .iter()
                .filter(|row| row.status == ItemStatus::Completed)
                .count();
            state.seq += 1;
            state.remux.count_text = format!("{done} of {} done", rows.len());
            state.remux.clock = format!("{}:{:02} elapsed", second / 60, second % 60);
            state.remux.progress.done = done as f64 * 100.0 / rows.len() as f64;
            state.remux.progress.live = started.len() as f64 * 100.0 / rows.len() as f64;
            state.remux.rows = Arc::new(rows.clone());
            let resetting = reset.load(Ordering::Relaxed) && in_flight.load(Ordering::Acquire) == 0;
            if resetting {
                state.remux.manifest_rev += 1;
                reset.store(false, Ordering::Relaxed);
            }
            publish(&state, resetting);
            std::thread::sleep(PUBLISH_INTERVAL);
        }
    });
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// `name_ms p50=… p99=… max=…` over `values`.
fn summary(name: &str, values: &mut [f64]) -> String {
    values.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        let last = values.len().saturating_sub(1);
        values
            .get((last as f64 * p).round() as usize)
            .copied()
            .unwrap_or(f64::NAN)
    };
    format!(
        "{name}_ms p50={:.2} p99={:.2} max={:.2}",
        pct(0.5),
        pct(0.99),
        pct(1.0)
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("skia-opengl".into())
        .select()?;
    let app = App::new()?;
    app.global::<Theme>().set_glass(false);
    publisher::install_models(&app);
    app.window().set_maximized(true);

    let rows = synthetic_rows(ROWS);
    let releases = rows.chunk_by(|a, b| a.release == b.release).count();
    let content = ROWS as f32 * ROW_H + releases as f32 * HEAD_H;
    let mut state = ViewState::new(None);
    state.remux.title = "Bench".into();
    state.remux.empty = None;
    state.remux.manifest_rev = 1;
    state.remux.library = library(Some("/media/anime"), &rows);
    state.remux.rows = Arc::new(rows);

    // SlintPublisher's push and schedule, with the hop timed: its time is the publication's
    // UI-thread time.
    let pending = Arc::new(Coalescer::default());
    let (edits, filter_edits) = (settings::Edits::default(), settings::Edits::default());
    let hops: Arc<Mutex<Vec<f64>>> = Arc::default();
    let maps: Arc<Mutex<Vec<f64>>> = Arc::default();
    let (window, timed, mapped) = (app.as_weak(), Arc::clone(&hops), Arc::clone(&maps));
    let (filtered, filter_window) = (Arc::clone(&pending), app.as_weak());
    let want_reset = Arc::new(AtomicBool::new(false));
    // The Run reset's hop: its UI-thread time and when it ended.
    let run_reset: Arc<Mutex<Option<(f64, Instant)>>> = Arc::default();
    let reset_hop = Arc::clone(&run_reset);
    // Hops scheduled and not yet finished: at 0 a publication schedules the hop that takes it. A
    // count, not a flag, as a hop that ends may have a later one queued behind it.
    let in_flight = Arc::new(AtomicUsize::new(0));
    spawn_churn(
        state,
        Arc::clone(&want_reset),
        Arc::clone(&in_flight),
        move |state, resetting| {
            let start = Instant::now();
            let scheduled = pending.push_state(state);
            mapped.lock().unwrap().push(ms(start.elapsed()));
            // Published only with no hop in flight, so the reset always schedules its own hop.
            debug_assert!(scheduled || !resetting);
            if scheduled {
                in_flight.fetch_add(1, Ordering::AcqRel);
                let (pending, window, hops, edits, done) = (
                    Arc::clone(&pending),
                    window.clone(),
                    Arc::clone(&timed),
                    Arc::clone(&edits),
                    Arc::clone(&in_flight),
                );
                let reset_hop = resetting.then(|| Arc::clone(&reset_hop));
                let queued = slint::invoke_from_event_loop(move || {
                    let start = Instant::now();
                    publisher::hop(&pending, &edits, &window);
                    let took = ms(start.elapsed());
                    hops.lock().unwrap().push(took);
                    if let Some(reset_hop) = reset_hop {
                        *reset_hop.lock().unwrap() = Some((took, Instant::now()));
                    }
                    done.fetch_sub(1, Ordering::AcqRel);
                });
                if queued.is_err() {
                    in_flight.fetch_sub(1, Ordering::AcqRel);
                }
            }
        },
    );

    let weak = app.as_weak();
    let (mut first, mut before, mut last) = (None::<Instant>, Instant::now(), None::<Instant>);
    let (mut intervals, mut renders) = (Vec::new(), Vec::new());
    let mut dir = 1.0;
    let (mut reported, mut reset) = (false, f64::NAN);
    // The Run reset is asked for once: a frame between the bump and its hop must not ask again.
    let mut reset_asked = false;
    // The render and interval of the frame the Run reset lands in.
    let mut reset_frame: Option<(f64, f64)> = None;
    app.window()
        .set_rendering_notifier(move |state, _| {
            match state {
                RenderingState::BeforeRendering => return before = Instant::now(),
                RenderingState::AfterRendering => {}
                _ => return,
            }
            let now = Instant::now();
            let (Some(app), false) = (weak.upgrade(), reported) else {
                return;
            };
            let since = now.duration_since(*first.get_or_insert(now));
            if since >= WARMUP {
                match last.replace(now) {
                    Some(last) => intervals.push(ms(now.duration_since(last))),
                    // The warm-up's publications do not count; the first is the reset.
                    None => {
                        let mut hops = hops.lock().unwrap();
                        reset = hops.first().copied().unwrap_or(f64::NAN);
                        hops.clear();
                        maps.lock().unwrap().clear();
                    }
                }
                renders.push(ms(now.duration_since(before)));
                if since >= WARMUP + WINDOW / 2 && !reset_asked {
                    reset_asked = true;
                    want_reset.store(true, Ordering::Relaxed);
                }
                if let (None, Some((_, at))) = (reset_frame, *run_reset.lock().unwrap()) {
                    if now > at {
                        let interval = intervals.last().copied().unwrap_or(f64::NAN);
                        reset_frame = Some((ms(now.duration_since(before)), interval));
                    }
                }
                // Bounce between the ends; the bottom is approximate (the list is shorter than the
                // window by its header), and 30 s at 6 px a frame (about 10.7k px, into the third
                // release) does not reach it anyway.
                let max = content - app.window().size().to_logical(app.window().scale_factor()).height;
                let y = (-app.get_remux_scroll() + STEP * dir).clamp(0.0, max.max(0.0));
                if y <= 0.0 || y >= max {
                    dir = -dir;
                }
                app.set_remux_scroll(-y);
            }
            if since >= WARMUP + WINDOW {
                reported = true;
                let over = |ms: f64| intervals.iter().filter(|&&i| i > ms).count();
                let (over_frame, over_vsync) = (over(16.7), over(25.0));
                let mut hops = std::mem::take(&mut *hops.lock().unwrap());
                let mut maps = std::mem::take(&mut *maps.lock().unwrap());
                // A filter change: kept and mapped on the UI thread, then the hop timed.
                filtered.set_filter(Mode::Remux, QueueFilter::Failed);
                filtered.remap(Mode::Remux);
                let start = Instant::now();
                publisher::hop(&filtered, &filter_edits, &filter_window);
                let filter = ms(start.elapsed());
                let verdict = |pass: bool| if pass { "PASS" } else { "FAIL" };
                let render_max = renders.iter().copied().fold(0.0, f64::max);
                let run_reset_ms = run_reset.lock().unwrap().map_or(f64::NAN, |(took, _)| took);
                let (reset_render, reset_interval) = reset_frame.unwrap_or((f64::NAN, f64::NAN));
                let pub_budget = hops.len() > 1 && {
                    let mut sorted = hops.clone();
                    sorted.sort_by(f64::total_cmp);
                    sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize] < 4.0
                };
                println!(
                    "BENCH session={} renderer=skia-opengl rows={ROWS} publications={} {} {} reset_ms={reset:.2} filter_ms={filter:.2} run_reset_ms={run_reset_ms:.2} run_reset_frame=render:{reset_render:.2},interval:{reset_interval:.2} frames={} {} over_16.7ms={over_frame} over_25ms={over_vsync} {} budget_pub={} budget_frame={}",
                    if std::env::var_os("WAYLAND_DISPLAY").is_some() { "wayland" } else { "x11" },
                    hops.len(),
                    summary("map", &mut maps),
                    summary("pub", &mut hops),
                    intervals.len(),
                    summary("interval", &mut intervals),
                    summary("render", &mut renders),
                    verdict(pub_budget),
                    verdict(render_max <= 16.7 && over_vsync == 0),
                );
                let _ = slint::quit_event_loop();
                return;
            }
            app.window().request_redraw();
        })
        .map_err(|error| format!("no rendering notifier: {error:?}"))?;
    app.run()?;
    Ok(())
}
