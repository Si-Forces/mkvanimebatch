import assert from "node:assert/strict";
import test from "node:test";

test("app forwards preflight options to Tauri while browse uses defaults", async t => {
  const previousDocument = globalThis.document;
  const previousWindow = globalThis.window;
  const calls = [];
  globalThis.document = {
    readyState: "loading",
    addEventListener() {},
    getElementById: () => null,
  };
  globalThis.window = {
    __TAURI__: { core: { invoke: async (command, args) => {
      calls.push([command, args]);
      return { version: 2, items: [], subtitle_candidates: {} };
    } } },
  };
  t.after(() => {
    globalThis.document = previousDocument;
    globalThis.window = previousWindow;
  });
  const { scanInputDirectory } = await import("./app.js");
  assert.equal(await scanInputDirectory("/release", "remux"), true);
  assert.deepEqual(calls[0], ["inspect_directory", { path: "/release", options: undefined }]);

  const options = { workers: 6, ioJobs: 0, noCache: true };
  assert.equal(await scanInputDirectory("/release", "remux", { force: true, options }), true);
  assert.deepEqual(calls[1], ["inspect_directory", { path: "/release", options }]);
});
