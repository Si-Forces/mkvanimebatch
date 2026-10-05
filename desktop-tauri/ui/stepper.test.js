import test from "node:test";
import assert from "node:assert/strict";

import { nextStepValue } from "./stepper.js";

test("an empty field with an auto value starts from it: + gives N+1, - gives N-1", () => {
  assert.equal(nextStepValue("", 6, 1, 1, 1, 64), 7);
  assert.equal(nextStepValue("", 6, -1, 1, 1, 64), 5);
  // A number input reports an unparsable entry as "", and a lone sign parses as NaN too.
  assert.equal(nextStepValue("-", 6, 1, 1, 1, 64), 7);
});

test("an empty field without an auto value starts from 0, as before", () => {
  for (const auto of [NaN, undefined, null]) {
    assert.equal(nextStepValue("", auto, 1, 1, 0, 64), 1);
    assert.equal(nextStepValue("", auto, -1, 1, 0, 64), 0);
    // FFmpeg threads (min 1): + lands on the minimum, - is clamped to it.
    assert.equal(nextStepValue("", auto, 1, 1, 1, 32), 1);
    assert.equal(nextStepValue("", auto, -1, 1, 1, 32), 1);
  }
  assert.equal(nextStepValue("", NaN, 1, 0.5, NaN, NaN), 0.5);
});

test("a non-empty value steps from itself and ignores the auto value", () => {
  assert.equal(nextStepValue("4", 6, 1, 1, 1, 64), 5);
  assert.equal(nextStepValue("4", 6, -1, 1, 1, 64), 3);
  assert.equal(nextStepValue(4, 6, 1, 1, 1, 64), 5);
  // A typed 0 is a value: the auto value does not replace it.
  assert.equal(nextStepValue("0", 6, 1, 1, 0, 64), 1);
  assert.equal(nextStepValue("0", 6, -1, 1, 0, 64), 0);
});

test("results are clamped to min and max", () => {
  assert.equal(nextStepValue("64", 6, 1, 1, 1, 64), 64);
  assert.equal(nextStepValue("1", 6, -1, 1, 1, 64), 1);
  assert.equal(nextStepValue("", 1, -1, 1, 1, 64), 1);
  assert.equal(nextStepValue("", 64, 1, 1, 1, 64), 64);
  // A typed value outside the bounds is pulled in, not just stepped.
  assert.equal(nextStepValue("100", 6, 1, 1, 1, 64), 64);
  assert.equal(nextStepValue("-5", 6, -1, 1, 1, 64), 1);
});

test("a missing min or max leaves that side unbounded", () => {
  assert.equal(nextStepValue("-3", NaN, -1, 1, NaN, NaN), -4);
  assert.equal(nextStepValue("1000", NaN, 1, 1, undefined, null), 1001);
});

test("fractional steps drop floating-point noise", () => {
  assert.equal(nextStepValue("0.2", NaN, 1, 0.1, NaN, NaN), 0.3);
  assert.equal(nextStepValue("0.3", NaN, -1, 0.1, NaN, NaN), 0.2);
});
