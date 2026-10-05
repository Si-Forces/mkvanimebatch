/**
 * Next value of a number input after one stepper click.
 *
 * A field that holds no number starts from `autoValue` when it is a finite number (the
 * engine's resolved default behind an "auto" placeholder), so + gives N+1 and − gives N−1.
 * Without one it starts from 0, as the steppers always did. A typed `0` is a value, not an
 * empty field, and steps from 0.
 *
 * `step`, `min` and `max` are numbers; a NaN, null or undefined `min` or `max` leaves that
 * side unbounded. The result is clamped, then rounded to 10 significant digits to drop
 * floating-point noise such as 0.30000000000000004.
 */
export function nextStepValue(current, autoValue, dir, step, min, max) {
  const typed = parseFloat(current);
  let val = Number.isNaN(typed) ? (Number.isFinite(autoValue) ? autoValue : 0) : typed;

  val += dir * step;
  if (min != null && !Number.isNaN(min) && val < min) val = min;
  if (max != null && !Number.isNaN(max) && val > max) val = max;

  return parseFloat(val.toPrecision(10));
}
