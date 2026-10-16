import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import {
  batchClock,
  candidateTier,
  filterCounts,
  formatElapsedDuration,
  formatOutcomeDuration,
  inspectorCrumbs,
  logDisplayText,
  matchesFilter,
  pathSegments,
  prettyPath,
  progressSegments,
  remuxAudioText,
  sparkPoints,
  statusLabel,
  statusTone,
  subtitleLadder,
  subtitleSourceText,
} from "./presentation.js";

test("paths read Finder-style: no slashes, no dot segments, Windows separators too", () => {
  assert.deepEqual(pathSegments("/media/anime/Show (2023)/"), ["media", "anime", "Show (2023)"]);
  assert.deepEqual(pathSegments("../Subs/./Show - SP1.ass"), ["Subs", "Show - SP1.ass"]);
  assert.deepEqual(pathSegments("C:\\Anime\\Show"), ["C:", "Anime", "Show"]);
  assert.deepEqual(pathSegments(null), []);
  assert.equal(prettyPath("/media/anime/Show"), "media › anime › Show");
  assert.equal(prettyPath("."), "");
});

test("the Inspector crumbs show the episode folder, not the file name the title already shows", () => {
  assert.deepEqual(inspectorCrumbs("Show - 01.mkv"), []);
  assert.deepEqual(inspectorCrumbs("Show/Season 1/Show - 01.mkv"), ["Show", "Season 1"]);
});

test("the log shows › for path separators, keeps digit fractions, and leaves the raw line alone", () => {
  const raw = "Remuxed: /media/anime/Season 1/Show - 01.mkv (3/12)";
  assert.equal(logDisplayText(raw), "Remuxed: media › anime › Season 1 › Show - 01.mkv (3/12)");
  assert.equal(logDisplayText("Output/Show.mkv"), "Output › Show.mkv");
  assert.equal(logDisplayText("C:\\Anime\\Show.mkv"), "C: › Anime › Show.mkv");
  assert.equal(logDisplayText("no paths here"), "no paths here");
  // A run of separators is one separator: between digits it stays whole, elsewhere one chevron.
  assert.equal(logDisplayText("16/9 and 2//1"), "16/9 and 2//1");
  assert.equal(logDisplayText("a//b C:\\Anime\\ trailing/"), "a › b C: › Anime ›  trailing › ");
  // Display is a pure function of the raw line: the caller still holds the original for Copy.
  assert.match(raw, /\/media\/anime\/Season 1\//);
});

test("status colors: running blue, done green, failed red, everything else muted", () => {
  assert.equal(statusTone("processing"), "info");
  assert.equal(statusTone("completed"), "ok");
  assert.equal(statusTone("error"), "err");
  for (const status of ["queued", "skipped", "cancelled", "planned", "unknown"]) assert.equal(statusTone(status), "mute");
});

test("status labels name the outcome, and a processing item its progress and elapsed time", () => {
  const now = 100_000;
  assert.equal(statusLabel({ status: "processing", startTime: now - 72_000, progress: 64.4 }, now), "64% · 1m 12s");
  assert.equal(statusLabel({ status: "processing", startTime: now - 6_000, progress: null }, now), "Processing · 6s");
  assert.equal(statusLabel({ status: "completed", durationSec: 48 }), "Done in 48.0s");
  assert.equal(statusLabel({ status: "completed", durationSec: null }), "Done");
  assert.equal(statusLabel({ status: "error" }), "Failed");
  assert.equal(statusLabel({ status: "skipped" }), "Skipped");
  assert.equal(statusLabel({ status: "queued" }), "Queued");
});

/** Queue state in the shape `QueueTracker.applyView` projects: `items` in queue order. */
function stateOf(items, batch = {}) {
  return {
    items: new Map(items.map(item => [item.id, { selected: true, status: "queued", progress: null, ...item }])),
    order: items.map(item => item.id),
    batchItemIds: null,
    batchStartTime: null,
    batchCompleted: false,
    batchSuccess: null,
    batchCancelled: false,
    ...batch,
  };
}

test("filters: To do, Done and Failed count selected items only, All counts every item", () => {
  const state = stateOf([
    { id: "a", status: "completed" },
    { id: "b", status: "error" },
    { id: "c", status: "processing" },
    { id: "d" },
    { id: "e", selected: false },
  ]);
  assert.deepEqual(filterCounts(state), { all: 5, todo: 2, done: 1, failed: 1 });
  assert.equal(matchesFilter(state.items.get("e"), "todo"), false);
  assert.equal(matchesFilter(state.items.get("e"), "all"), true);
});

test("progress segments cover the Active Batch, and the clock says how the batch ended", () => {
  const ids = ["a", "b", "c", "d"];
  assert.equal(batchClock(stateOf(ids.map(id => ({ id })))), "Not started");
  const running = stateOf([
    { id: "a", status: "completed" },
    { id: "b", status: "skipped" },
    { id: "c", status: "processing", progress: 40 },
    { id: "d" },
    // Not a participant: the Active Batch is the four above.
    { id: "e", status: "error" },
  ], { batchItemIds: ids, batchStartTime: 1_000 });
  assert.deepEqual(progressSegments(running), { done: 25, skipped: 25, failed: 0, live: 10 });
  assert.equal(batchClock(running, 73_000), "1m 12s elapsed");
  const ended = { ...running, batchCompleted: true, batchSuccess: false };
  assert.equal(batchClock(ended), "Finished with errors");
  assert.equal(batchClock({ ...ended, batchCancelled: true }), "Stopped");
  assert.equal(batchClock({ ...ended, batchSuccess: true }), "Finished");
});

test("formatOutcomeDuration formats seconds across the sub-0.1s, seconds, and minutes thresholds", () => {
  assert.equal(formatOutcomeDuration(0), "<0.1s");
  assert.equal(formatOutcomeDuration(0.04), "<0.1s");
  assert.equal(formatOutcomeDuration(4.2), "4.2s");
  assert.equal(formatOutcomeDuration(59.94), "59.9s");
  assert.equal(formatOutcomeDuration(59.96), "1m 0.0s");
  assert.equal(formatOutcomeDuration(60.0), "1m 0.0s");
  assert.equal(formatOutcomeDuration(75.4), "1m 15.4s");
  assert.equal(formatOutcomeDuration(119.96), "2m 0.0s");
});

test("formatElapsedDuration formats negative seconds, seconds, and minutes", () => {
  assert.equal(formatElapsedDuration(-5), "0s");
  assert.equal(formatElapsedDuration(-0.1), "0s");
  assert.equal(formatElapsedDuration(0), "0s");
  assert.equal(formatElapsedDuration(4.2), "4s");
  assert.equal(formatElapsedDuration(42), "42s");
  assert.equal(formatElapsedDuration(59.9), "59s");
  assert.equal(formatElapsedDuration(75.4), "1m 15s");
  assert.equal(formatElapsedDuration(130), "2m 10s");
});

test("remux audio names the Opus bitrate only for FLAC", () => {
  assert.equal(remuxAudioText({ audio_summary: "FLAC 2.0", has_flac: true }, "192k"), "FLAC 2.0 → Opus 192k");
  assert.equal(remuxAudioText({ audio_summary: "AAC 2.0", has_flac: false }, "192k"), "AAC 2.0 · passthrough");
  assert.equal(remuxAudioText(null, "192k"), "No audio");
});

test("the subtitle ladder follows the precedence order and never claims an automatic file", () => {
  const candidates = ["Season 1/Show - 04.ass", "Show - 01.ass", "Subs/Show - 02.ass"];
  const auto = subtitleLadder({ release: "Season 1", candidates, embedded: "ASS (ENG)" });
  assert.deepEqual(auto.map(r => [r.key, r.tag]), [
    ["manual", "None"], ["same", "Auto"], ["embedded", "Found"], ["ancestor", "Found"], ["assets", "None"],
  ]);
  assert.equal(auto[1].detail, "Season 1/Show - 04.ass");
  assert.equal(auto[3].detail, "2 subtitle files");
  assert.equal(subtitleSourceText(auto), "Same folder subs");

  const manual = subtitleLadder({ release: "Season 1", candidates, assignment: { asset_path: "/x/fixed.ass" } });
  assert.equal(manual[0].tag, "Used");
  assert.equal(manual[1].tag, "Found");
  assert.equal(subtitleSourceText(manual), "Manual subs");

  // The root release lists only root files as "same folder".
  const root = subtitleLadder({ release: ".", candidates: ["Show - 01.ass"] });
  assert.equal(root[1].tag, "Auto");
  assert.equal(subtitleSourceText(subtitleLadder({ release: "." })), "No subs");
  // A configured assets dir is a fallback setting, never a found file.
  const assetsOnly = subtitleLadder({ release: ".", assetsDir: "/assets" });
  assert.equal(assetsOnly[4].tag, "Fallback");
  assert.equal(assetsOnly.some(rung => rung.chosen), false);
  assert.equal(subtitleSourceText(assetsOnly), "Assets dir fallback");

  assert.equal(candidateTier("Season 1/Show - 04.ass", "Season 1"), "same");
  assert.equal(candidateTier("Show - 01.ass", "Season 1"), "ancestor");
  assert.equal(candidateTier("Show - 01.ass", "."), "same");
  assert.equal(candidateTier("Show - 01.ass", ""), "same");
});

test("sparkline points scale to the box and clamp out-of-range samples", () => {
  assert.equal(sparkPoints([5], 100), "");
  assert.equal(sparkPoints([0, 100, 200], 100, 64, 20), "0.0,18.0 32.0,2.0 64.0,2.0");
});

// WebKitGTK before 2.40 cannot parse a regex lookbehind, and the SyntaxError stops the whole
// module graph from loading, so the window stays empty. The deb and rpm use the host's WebKitGTK.
test("no UI module uses a regex lookbehind", () => {
  const dir = new URL(".", import.meta.url);
  for (const file of fs.readdirSync(dir).filter(name => name.endsWith(".js") && !name.endsWith(".test.js"))) {
    assert.doesNotMatch(fs.readFileSync(new URL(file, dir), "utf8"), /\(\?<[=!]/, file);
  }
});
