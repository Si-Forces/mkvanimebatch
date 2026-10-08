import assert from "node:assert/strict";
import test from "node:test";
import { createDirectoryScanner } from "./directory-scan.js";

/** A promise the test settles by hand. */
function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}

const flush = () => new Promise(resolve => setImmediate(resolve));

/** The `canonical_directory` of a folder whose `/release/` and `/release/.` spellings are one. */
const trimDotSpellings = ({ path }) => path.replace(/\/\.?$/, "");

/**
 * A Queue Tracker with only the scan methods the scanner calls. Calls go to `log` as
 * `[mode, method, ...args]`. Request numbers count as in the real tracker, and `reset()`
 * leaves them alone.
 */
function fakeTracker(mode, log, reuses) {
  let request = 0;
  let token = 0;
  return {
    beginScanRequest: () => ++request,
    isNewestScanRequest: candidate => candidate === request,
    reset: () => log.push([mode, "reset"]),
    reuseInspection(directory) {
      log.push([mode, "reuseInspection", directory]);
      return reuses(directory);
    },
    startScan(directory) {
      log.push([mode, "startScan", directory]);
      return ++token;
    },
    acceptInspection(scanToken, directory, manifest) {
      log.push([mode, "acceptInspection", scanToken, directory, manifest]);
      return true;
    },
    rejectInspection: scanToken => log.push([mode, "rejectInspection", scanToken]),
  };
}

/**
 * A scanner over a fake `invoke` and one fake tracker per mode. `canonical` and `inspect` answer
 * the two commands from their arguments (a promise or a throw is fine); `log` holds every tracker
 * call and every `onSettled` as `["settled"]`, in order.
 */
function scannerHarness({
  canonical = ({ path }) => path,
  inspect = ({ path }) => ({ version: 2, items: [], canonical_dir: path }),
  reuses = () => false,
} = {}) {
  const calls = [];
  const log = [];
  const invoke = async (command, args) => {
    calls.push([command, args]);
    if (command === "canonical_directory") return canonical(args);
    if (command === "inspect_directory") return inspect(args);
    throw new Error(`unexpected command: ${command}`);
  };
  const trackers = { remux: fakeTracker("remux", log, reuses), extract: fakeTracker("extract", log, reuses) };
  const scan = createDirectoryScanner({
    invoke,
    queueFor: mode => trackers[mode],
    onSettled: () => log.push(["settled"]),
  });
  const argsOf = command => calls.filter(([name]) => name === command).map(([, args]) => args);
  const logged = method => log.filter(([, name]) => name === method);
  return { scan, calls, log, logged, resolved: () => argsOf("canonical_directory"), inspected: () => argsOf("inspect_directory") };
}

/** A harness whose `canonical_directory` answers `/slow` only once `release()` is called. */
function slowCanonicalHarness() {
  const slow = deferred();
  const harness = scannerHarness({ canonical: ({ path }) => (path === "/slow" ? slow.promise : path) });
  return { ...harness, release: () => slow.resolve("/slow") };
}

test("preflight options reach inspect_directory, and a scan without them sends none", async () => {
  const { scan, calls, inspected } = scannerHarness();
  assert.equal(await scan("/release", "remux"), true);
  assert.deepEqual(calls[0], ["canonical_directory", { path: "/release" }]);
  assert.deepEqual(inspected(), [{ path: "/release", options: undefined }]);

  const options = { noCache: true };
  assert.equal(await scan("/release", "remux", { force: true, options }), true);
  assert.deepEqual(inspected().at(-1), { path: "/release", options });
});

test("the Directory Identity, not the typed spelling, is reused, scanned and inspected", async () => {
  const { scan, logged, resolved, inspected } = scannerHarness({ canonical: trimDotSpellings });
  assert.equal(await scan(" /release ", "remux"), true);
  assert.equal(await scan("/release/", "remux"), true);
  assert.equal(await scan("/release/.", "remux"), true);

  // The typed path is trimmed, then resolved; everything after that sees the identity.
  assert.deepEqual(resolved(), [{ path: "/release" }, { path: "/release/" }, { path: "/release/." }]);
  assert.deepEqual(logged("reuseInspection").map(([, , directory]) => directory), ["/release", "/release", "/release"]);
  assert.deepEqual(logged("startScan").map(([, , directory]) => directory), ["/release", "/release", "/release"]);
  assert.deepEqual(inspected().map(({ path }) => path), ["/release", "/release", "/release"]);
});

test("a manifest without canonical_dir is accepted under the identity, and one with it under its own", async () => {
  const bare = { version: 2, items: [] };
  const named = { version: 2, items: [], canonical_dir: "/canonical" };
  const answers = [bare, named];
  const { scan, logged } = scannerHarness({ canonical: trimDotSpellings, inspect: () => answers.shift() });

  assert.equal(await scan("/release/.", "remux"), true);
  assert.equal(await scan("/release/.", "remux", { force: true }), true);

  assert.deepEqual(logged("acceptInspection").map(([, , , directory, manifest]) => [directory, manifest]), [
    ["/release", bare],
    ["/canonical", named],
  ]);
});

test("a reusable inspection answers without inspecting, and a forced scan never asks to reuse", async () => {
  const { scan, log, inspected } = scannerHarness({ reuses: () => true });

  assert.equal(await scan("/release", "remux"), true);
  assert.deepEqual(log, [["remux", "reuseInspection", "/release"], ["settled"]]);
  assert.deepEqual(inspected(), []);

  assert.equal(await scan("/release", "remux", { force: true }), true);
  assert.equal(log.filter(([, method]) => method === "reuseInspection").length, 1);
  assert.deepEqual(inspected(), [{ path: "/release", options: undefined }]);
});

test("a forced scan starts a fresh inspection even while an identical one is pending", async () => {
  const first = deferred();
  const second = deferred();
  const answers = [first.promise, second.promise];
  const { scan, inspected } = scannerHarness({ inspect: () => answers.shift() });

  const pending = scan("/release", "remux");
  await flush();
  const forced = scan("/release", "remux", { force: true });
  await flush();
  assert.equal(inspected().length, 2);

  first.resolve({ version: 2, items: [] });
  second.resolve({ version: 2, items: [] });
  await Promise.all([pending, forced]);
});

test("an empty path resets the queue without asking the bridge", async () => {
  const { scan, calls, log } = scannerHarness();
  assert.equal(await scan("   ", "extract"), false);
  assert.deepEqual(calls, []);
  assert.deepEqual(log, [["extract", "reset"], ["settled"]]);
});

test("concurrent scans of two spellings inspect the identity once, and the newer request is the one accepted", async () => {
  const { scan, logged, inspected } = scannerHarness({ canonical: trimDotSpellings });

  const results = await Promise.all([scan("/release/", "remux"), scan("/release/.", "remux")]);

  assert.deepEqual(results, [false, true]);
  assert.deepEqual(inspected(), [{ path: "/release", options: undefined }]);
  assert.deepEqual(logged("startScan"), [["remux", "startScan", "/release"]]);
  assert.equal(logged("acceptInspection").length, 1);
});

test("one inspection in flight is shared by the scans of both views", async () => {
  const inspection = deferred();
  const { scan, logged, inspected } = scannerHarness({ inspect: () => inspection.promise });

  const remux = scan("/release", "remux");
  const extract = scan("/release", "extract");
  await flush();
  assert.equal(inspected().length, 1);

  inspection.resolve({ version: 2, items: [] });
  assert.deepEqual(await Promise.all([remux, extract]), [true, true]);
  assert.deepEqual(logged("acceptInspection").map(([mode]) => mode).sort(), ["extract", "remux"]);
  assert.equal(inspected().length, 1);
});

test("a scan whose canonicalization answers late does not overtake a newer request", async () => {
  const { scan, release, logged, inspected } = slowCanonicalHarness();

  const overtaken = scan("/slow", "remux");
  assert.equal(await scan("/fast", "remux"), true);
  release();

  assert.equal(await overtaken, false);
  assert.deepEqual(inspected().map(({ path }) => path), ["/fast"]);
  assert.deepEqual(logged("startScan"), [["remux", "startScan", "/fast"]]);
});

test("clearing the input cancels a scan still waiting for its canonicalization", async () => {
  const { scan, release, log, inspected } = slowCanonicalHarness();

  const pending = scan("/slow", "remux");
  assert.equal(await scan("   ", "remux"), false);
  release();

  assert.equal(await pending, false);
  assert.deepEqual(inspected(), []);
  assert.deepEqual(log, [["remux", "reset"], ["settled"]]);
});

test("a pending scan of one view is not cancelled by a scan of the other", async () => {
  const { scan, release, logged, inspected } = slowCanonicalHarness();

  const remux = scan("/slow", "remux");
  assert.equal(await scan("/fast", "extract"), true);
  release();

  assert.equal(await remux, true);
  assert.deepEqual(inspected().map(({ path }) => path).sort(), ["/fast", "/slow"]);
  assert.deepEqual(logged("acceptInspection").map(([mode]) => mode).sort(), ["extract", "remux"]);
});

test("a path the bridge cannot resolve is inspected as typed, and the inspect error rejects the scan", async t => {
  const failure = new Error("Invalid directory path: No such file or directory (os error 2)");
  const logged = t.mock.method(console, "log", () => {});
  const { scan, log, inspected } = scannerHarness({
    canonical: () => { throw failure; },
    inspect: () => { throw failure; },
  });

  assert.equal(await scan(" /gone/ ", "remux"), false);

  assert.deepEqual(inspected(), [{ path: "/gone/", options: undefined }]);
  assert.deepEqual(log, [
    ["remux", "reuseInspection", "/gone/"],
    ["remux", "startScan", "/gone/"],
    ["settled"],
    ["remux", "rejectInspection", 1],
    ["settled"],
  ]);
  assert.deepEqual(logged.mock.calls.at(-1).arguments, ["[inspect_directory] failed:", failure]);
});

test("onSettled runs when a scan starts and again when it ends, and not for an overtaken request", async () => {
  const { scan, release, log } = slowCanonicalHarness();
  const settledCount = () => log.filter(([name]) => name === "settled").length;

  assert.equal(await scan("/fast", "remux"), true);
  assert.deepEqual(log.map(([name, method]) => method ?? name), ["reuseInspection", "startScan", "settled", "acceptInspection", "settled"]);

  // The overtaken request leaves the controls to the newer one.
  const before = settledCount();
  const overtaken = scan("/slow", "remux");
  await scan("/fast", "remux");
  const afterNewer = settledCount();
  assert.equal(afterNewer, before + 2);
  release();
  assert.equal(await overtaken, false);
  assert.equal(settledCount(), afterNewer);
});
