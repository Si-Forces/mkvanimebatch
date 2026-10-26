//! System Telemetry in the bottom bar (P6): the core's CPU, RAM and disk write texts and its sample
//! history (`mkv_desktop_core::telemetry`, which also sets the cadence), drawn as sparklines whose
//! geometry is the host's (presentation.js `sparkPoints`); and whether the window shows, which the
//! core is told (`Intent::ViewVisible`, app.js `sendVisibility`) so it samples nothing while hidden.

use mkv_anime_batch_slint_ui::{App, Telemetry};
use mkv_desktop_core::telemetry::TelemetryView;
use slint::winit_030::winit::event::WindowEvent;
use std::fmt::Write;

/// A sparkline's box, as index.html's `<svg width="64" height="20">`.
const SPARK_WIDTH: f32 = 64.0;
const SPARK_HEIGHT: f32 = 20.0;
/// The disk line's scale floor in MB/s, so a few MB/s of idle writes stay near the bottom.
const DISK_FLOOR: f32 = 50.0;

/// The sparkline of `samples` (oldest first) scaled to `max`, as `Path` commands in the 64x20 box:
/// evenly spaced, each value clamped to 0..=max and drawn between 2 px from the bottom and 2 px
/// from the top. Empty for fewer than two samples (no line), as `sparkPoints`.
fn spark_commands<'a>(samples: impl ExactSizeIterator<Item = &'a f32>, max: f32) -> String {
    let count = samples.len();
    if count < 2 {
        return String::new();
    }
    // At most SPARK_SAMPLES (40) points: exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let step = SPARK_WIDTH / (count - 1) as f32;
    let mut commands = String::with_capacity(count * 12);
    for (i, &value) in samples.enumerate() {
        // A NaN would break the whole path; the core never sends one, but read it as 0.
        let share = if value.is_finite() { value / max } else { 0.0 };
        #[allow(clippy::cast_precision_loss)]
        let x = i as f32 * step;
        let y = SPARK_HEIGHT - 2.0 - share.clamp(0.0, 1.0) * (SPARK_HEIGHT - 4.0);
        let verb = if i == 0 { 'M' } else { 'L' };
        let _ = write!(commands, "{verb}{x:.1} {y:.1} ");
    }
    commands.truncate(commands.trim_end().len());
    commands
}

/// The bottom bar's telemetry: the core's texts, CPU and RAM lines in percent, the disk line
/// scaled to its own peak, at least [`DISK_FLOOR`].
pub(crate) fn map(view: &TelemetryView) -> Telemetry {
    let samples = &view.samples;
    let disk_max = samples.disk.iter().copied().fold(DISK_FLOOR, f32::max);
    Telemetry {
        cpu: view.cpu.as_str().into(),
        ram: view.ram.as_str().into(),
        disk: view.disk.as_str().into(),
        cpu_line: spark_commands(samples.cpu.iter(), 100.0).into(),
        ram_line: spark_commands(samples.ram.iter(), 100.0).into(),
        disk_line: spark_commands(samples.disk.iter(), disk_max).into(),
    }
}

/// Shows `view`, on the event loop; nothing until the first sample, and only when it changed.
pub(crate) fn show(app: &App, view: Option<&TelemetryView>) {
    if let Some(view) = view {
        let telemetry = map(view);
        if app.get_telemetry() != telemetry {
            app.set_telemetry(telemetry);
        }
    }
}

/// Whether the window shows, from what winit reports: hidden while occluded (X11), while its size
/// is zero (a minimized window on Windows, which sends no `Occluded`: Slint's
/// `winitwindowadapter.rs` works around the same), or while minimized as `Window::is_minimized`
/// polls it (X11 and Windows). On Wayland winit 0.30 reports none of these, so the window counts as
/// shown there.
#[derive(Debug, Default)]
pub(crate) struct Visibility {
    occluded: bool,
    zero_size: bool,
    minimized: bool,
}

impl Visibility {
    fn visible(&self) -> bool {
        !(self.occluded || self.zero_size || self.minimized)
    }

    /// Applies `change`; answers the new visibility when it changed.
    fn change(&mut self, change: impl FnOnce(&mut Self)) -> Option<bool> {
        let was = self.visible();
        change(self);
        let now = self.visible();
        (now != was).then_some(now)
    }

    /// Takes in a window event; answers the new visibility when it changed.
    pub(crate) fn event(&mut self, event: &WindowEvent) -> Option<bool> {
        match event {
            WindowEvent::Occluded(occluded) => self.change(|v| v.occluded = *occluded),
            WindowEvent::Resized(size) => {
                self.change(|v| v.zero_size = size.width == 0 || size.height == 0)
            }
            _ => None,
        }
    }

    /// Takes in a poll of `is_minimized` (`None`: the platform cannot tell, read as shown).
    pub(crate) fn minimized(&mut self, minimized: Option<bool>) -> Option<bool> {
        self.change(|v| v.minimized = minimized == Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::winit_030::winit::dpi::PhysicalSize;
    use std::collections::VecDeque;

    #[test]
    fn a_spark_spreads_its_samples_across_the_box_between_its_2px_margins() {
        // presentation.js sparkPoints([0, 50, 100], 100): "0.0,18.0 32.0,10.0 64.0,2.0".
        let samples = VecDeque::from([0.0, 50.0, 100.0]);
        assert_eq!(
            spark_commands(samples.iter(), 100.0),
            "M0.0 18.0 L32.0 10.0 L64.0 2.0"
        );
        // Out of range clamps; a NaN reads 0.
        let samples = VecDeque::from([-5.0, 250.0, f32::NAN, 25.0]);
        assert_eq!(
            spark_commands(samples.iter(), 100.0),
            "M0.0 18.0 L21.3 2.0 L42.7 18.0 L64.0 14.0"
        );
        // Fewer than two samples draw no line.
        assert_eq!(spark_commands(VecDeque::from([3.0]).iter(), 100.0), "");
        assert_eq!(spark_commands(VecDeque::new().iter(), 100.0), "");
    }

    #[test]
    fn the_disk_line_scales_to_its_peak_but_never_below_50_mb_s() {
        let mut view = TelemetryView {
            cpu: "37%".into(),
            ram: "5.12 GB".into(),
            disk: "Idle".into(),
            ..TelemetryView::default()
        };
        view.samples.cpu = VecDeque::from([10.0, 20.0]);
        view.samples.ram = VecDeque::from([50.0, 50.0]);
        view.samples.disk = VecDeque::from([5.0, 25.0]);
        let shown = map(&view);
        assert_eq!(
            (shown.cpu.as_str(), shown.ram.as_str(), shown.disk.as_str()),
            ("37%", "5.12 GB", "Idle")
        );
        assert_eq!(shown.cpu_line, "M0.0 16.4 L64.0 14.8");
        assert_eq!(shown.ram_line, "M0.0 10.0 L64.0 10.0");
        // Under the floor: 25 MB/s is half way up.
        assert_eq!(shown.disk_line, "M0.0 16.4 L64.0 10.0");
        // Over it: the peak is the top.
        view.samples.disk = VecDeque::from([100.0, 200.0]);
        assert_eq!(map(&view).disk_line, "M0.0 10.0 L64.0 2.0");
    }

    #[test]
    fn the_window_is_hidden_while_occluded_minimized_or_sized_to_zero() {
        let mut shown = Visibility::default();
        let resized = |w, h| WindowEvent::Resized(PhysicalSize::new(w, h));
        // Nothing changes while shown.
        assert_eq!(shown.event(&resized(920, 640)), None);
        assert_eq!(shown.event(&WindowEvent::Occluded(false)), None);
        assert_eq!(shown.minimized(None), None);
        assert_eq!(shown.event(&WindowEvent::Focused(false)), None);
        // Occluded (X11), then shown again.
        assert_eq!(shown.event(&WindowEvent::Occluded(true)), Some(false));
        assert_eq!(shown.event(&WindowEvent::Occluded(true)), None);
        assert_eq!(shown.event(&WindowEvent::Occluded(false)), Some(true));
        // Minimized on Windows: a zero size, then the restore's size.
        assert_eq!(shown.event(&resized(0, 0)), Some(false));
        assert_eq!(shown.event(&resized(920, 640)), Some(true));
        // Minimized as polled; hidden for as long as any one says so.
        assert_eq!(shown.minimized(Some(true)), Some(false));
        assert_eq!(shown.event(&WindowEvent::Occluded(true)), None);
        assert_eq!(shown.minimized(Some(false)), None);
        assert_eq!(shown.event(&WindowEvent::Occluded(false)), Some(true));
    }
}
