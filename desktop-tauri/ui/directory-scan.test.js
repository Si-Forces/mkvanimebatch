// The scan rules themselves (Directory Identity, Inspection Reuse, shared and overtaken scans,
// resets) live in the Desktop Core and are tested in `crates/mkv-desktop-core/src/actor.rs`.
// These cases cover only what the webview adds: the Intent it sends and what it does with the reply.
import assert from "node:assert/strict";
import test from "node:test";
import { createDirectoryScanner } from "./directory-scan.js";

/** A scanner over a fake `invoke` whose `dispatch` answers with `reply` (a value, or a function of the intent). */
function scannerHarness(reply = true) {
  const log = [];
  const invoke = async (command, args) => {
    log.push([command, args]);
    if (command !== "dispatch") throw new Error(`unexpected command: ${command}`);
    return typeof reply === "function" ? reply(args.intent) : reply;
  };
  const scan = createDirectoryScanner({
    invoke,
    syncState: async () => log.push(["sync"]),
    onSettled: () => log.push(["settled"]),
  });
  return { scan, log };
}

test("a scan sends the ScanDirectory Intent, with the preflight's force and no_cache when given", async () => {
  const { scan, log } = scannerHarness();
  assert.equal(await scan(" /release ", "remux"), true);
  assert.equal(await scan("/release", "extract", { force: true, noCache: true }), true);

  assert.deepEqual(log.filter(([command]) => command === "dispatch").map(([, args]) => args.intent), [
    // The core trims and resolves the path itself.
    { type: "scan_directory", mode: "remux", path: " /release ", force: false, no_cache: false },
    { type: "scan_directory", mode: "extract", path: "/release", force: true, no_cache: true },
  ]);
});

test("the core's reply is the scan's answer: false for an overtaken, failed or reset scan", async () => {
  const { scan } = scannerHarness(intent => intent.path === "/accepted");
  assert.equal(await scan("/accepted", "remux"), true);
  assert.equal(await scan("/overtaken", "remux"), false);
  assert.equal(await scan("", "remux"), false);
});

test("the published state is applied before the scan answers, then the controls catch up", async () => {
  const { scan, log } = scannerHarness();
  await scan("/release", "remux");
  assert.deepEqual(log.map(([name]) => name), ["dispatch", "sync", "settled"]);
});

test("a refused or failed dispatch answers false, and still applies the state and settles", async t => {
  const failure = "The Desktop Core is not running.";
  const logged = t.mock.method(console, "error", () => {});
  const { scan, log } = scannerHarness(() => { throw failure; });

  assert.equal(await scan("/release", "remux"), false);

  assert.deepEqual(log.map(([name]) => name), ["dispatch", "sync", "settled"]);
  assert.deepEqual(logged.mock.calls.at(-1).arguments, ["[scan_directory] failed:", failure]);
});
