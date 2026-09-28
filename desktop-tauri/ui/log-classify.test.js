import test from "node:test";
import assert from "node:assert/strict";
import { classifyLine, stripLogPrefix } from "./log-classify.js";

test("stripLogPrefix removes timestamp and level, keeps [ERR] marker", () => {
  assert.equal(stripLogPrefix("2026-08-23 10:12:01,123 [INFO] Remuxed: a.mkv"), "Remuxed: a.mkv");
  assert.equal(stripLogPrefix("2026-08-23 10:12:01,000 [INFO] Done"), "Done");
  assert.equal(stripLogPrefix("[ERR] 2026-08-23 10:12:01,000 [ERROR] mkvmerge failed"), "[ERR] mkvmerge failed");
  // Timestamps without fractional seconds are not engine-formatted; left intact.
  assert.equal(stripLogPrefix("2026-08-23 10:12:01 [INFO] Odd"), "2026-08-23 10:12:01 [INFO] Odd");
  assert.equal(stripLogPrefix("plain line"), "plain line");
});

test("error wording classifies as line-err", () => {
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [INFO] CRASHED: Show - 01.mkv"), "line-err");
  assert.equal(classifyLine("[ERR] 2026-08-23 10:00:00,000 [ERROR] mkvmerge failed (exit 2)"), "line-err");
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [INFO]   !! ERROR: no video track"), "line-err");
});

test("skip wording classifies as line-skip", () => {
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [WARNING] SKIPPED: Show - 01.mkv | Reason: no subtitle files found"), "line-skip");
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [INFO] Output already exists, skipping."), "line-skip");
});

test("success wording classifies as line-ok", () => {
  assert.equal(classifyLine("2026-08-23 10:12:01,123 [INFO] Remuxed: Show - 01.mkv"), "line-ok");
  assert.equal(classifyLine("2026-08-23 10:12:01,123 [INFO] Extracted cleanly: fonts"), "line-ok");
  assert.equal(classifyLine("Batch finished successfully."), "line-ok");
});

test("warning wording classifies as line-warn", () => {
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [WARNING] mkvmerge emitted warnings"), "line-warn");
});

test("info wording classifies as line-info", () => {
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [INFO] Processing directory: Season 01"), "line-info");
  assert.equal(classifyLine("--- DRY RUN: Planned jobs ---"), "line-info");
  assert.equal(classifyLine("Fonts directory: /x/fonts"), "line-info");
  assert.equal(classifyLine("Extracting attachments from Show.mkv"), "line-info");
});

test("cache wording classifies as line-done", () => {
  assert.equal(classifyLine("2026-08-23 10:00:00,000 [INFO] Persistent inspection cache saved."), "line-done");
  assert.equal(classifyLine("Persistent inspection cache cleared."), "line-done");
});

test("unrecognised lines classify to empty string", () => {
  assert.equal(classifyLine(""), "");
  assert.equal(classifyLine("random text with no keywords"), "");
});
