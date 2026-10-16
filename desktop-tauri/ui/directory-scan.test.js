// The scan rules themselves (Directory Identity, Inspection Reuse, shared and overtaken scans,
// resets) live in the Desktop Core and are tested in `crates/mkv-desktop-core/src/actor.rs`.
// These cases cover only what the webview adds: the Intent it sends and what it does with the reply.
import assert from "node:assert/strict";
import test from "node:test";
import { createDirectoryScanner } from "./directory-scan.js";

/** A scanner over a fake `dispatch` that answers with `reply` (a value, or a function of the intent). */
function scannerHarness(reply = true) {
  const log = [];
  const dispatch = async intent => {
    log.push(["dispatch", intent]);
    return typeof reply === "function" ? reply(intent) : reply;
  };
  const scan = createDirectoryScanner({ dispatch, onSettled: () => log.push(["settled"]) });
  return { scan, log };
}

test("a scan sends the ScanDirectory Intent, with the preflight's force and no_cache when given", async () => {
  const { scan, log } = scannerHarness();
  assert.equal(await scan(" /release ", "remux"), true);
  assert.equal(await scan("/release", "extract", { force: true, noCache: true }), true);

  assert.deepEqual(log.filter(([name]) => name === "dispatch").map(([, intent]) => intent), [
    // The core trims and resolves the path itself.
    { type: "scan_directory", mode: "remux", path: " /release ", force: false, no_cache: false },
    { type: "scan_directory", mode: "extract", path: "/release", force: true, no_cache: true },
  ]);
});

test("the core's reply is the scan's answer: false for an overtaken, failed, reset or refused scan", async () => {
  const { scan } = scannerHarness(intent => intent.path === "/accepted");
  assert.equal(await scan("/accepted", "remux"), true);
  assert.equal(await scan("/overtaken", "remux"), false);
  assert.equal(await scan("", "remux"), false);
});

test("the controls catch up once the dispatch, which applied the published state, settled", async () => {
  const { scan, log } = scannerHarness();
  await scan("/release", "remux");
  assert.deepEqual(log.map(([name]) => name), ["dispatch", "settled"]);
});
