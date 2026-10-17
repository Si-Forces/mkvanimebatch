/**
 * The telemetry sparklines' drawing, the one presentation rule the view keeps. Every displayed
 * text and number (labels, tones, the clock, the counts, the ladder) is the Desktop Core's, in
 * `crates/mkv-desktop-core/src/presentation.rs` and `inspector.rs`.
 */

/**
 * A sparkline's polyline points for `samples` scaled to `max`, newest at the right edge of a
 * `width`×`height` box. Returns "" with fewer than two samples.
 */
export function sparkPoints(samples, max, width = 64, height = 20) {
  if (samples.length < 2) return "";
  const step = width / (samples.length - 1);
  return samples
    .map((value, i) => `${(i * step).toFixed(1)},${(height - 2 - Math.min(Math.max(value / max, 0), 1) * (height - 4)).toFixed(1)}`)
    .join(" ");
}
