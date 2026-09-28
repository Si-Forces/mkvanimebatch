import assert from "node:assert/strict";
import test from "node:test";

import { createInspectionCoordinator } from "./inspection-coordinator.js";

test("inspection coordinator shares only the current same-path request", async () => {
  const resolvers = [];
  const paths = [];
  const inspect = createInspectionCoordinator(path => new Promise(resolve => {
    paths.push(path);
    resolvers.push(resolve);
  }));

  const first = inspect("/release");
  const shared = inspect("/release");
  await Promise.resolve();
  assert.deepEqual(paths, ["/release"]);

  resolvers[0]({ version: 2, items: [] });
  assert.deepEqual(await first, await shared);

  const replacement = inspect("/release");
  await Promise.resolve();
  assert.deepEqual(paths, ["/release", "/release"]);
  resolvers[1]({ version: 2, items: [{ id: "01.mkv" }] });
  assert.equal((await replacement).items.length, 1);
});

test("settling one path keeps another path's pending request shared", async () => {
  const pending = new Map();
  const inspect = createInspectionCoordinator(path => new Promise(resolve => pending.set(path, resolve)));

  const first = inspect("/old");
  const current = inspect("/new");
  await Promise.resolve();
  pending.get("/old")("old");
  assert.equal(await first, "old");

  const shared = inspect("/new");
  pending.get("/new")("new");
  assert.equal(await current, "new");
  assert.equal(await shared, "new");
});

test("same-path requests share even when another path starts between them", async () => {
  const resolvers = new Map();
  const paths = [];
  const inspect = createInspectionCoordinator(path => new Promise(resolve => {
    paths.push(path);
    resolvers.set(path, resolve);
  }));

  const first = inspect("/first");
  const other = inspect("/other");
  const shared = inspect("/first");
  await Promise.resolve();

  assert.deepEqual(paths, ["/first", "/other"]);
  resolvers.get("/first")("first");
  resolvers.get("/other")("other");
  assert.equal(await first, "first");
  assert.equal(await shared, "first");
  assert.equal(await other, "other");
});

test("a forced inspection never shares a pending browse inspection", async () => {
  const resolvers = [];
  const paths = [];
  const inspect = createInspectionCoordinator(path => new Promise(resolve => {
    paths.push(path);
    resolvers.push(resolve);
  }));

  const browse = inspect("/release");
  const preflight = inspect("/release", { force: true });
  const sharedPreflight = inspect("/release");
  await Promise.resolve();

  assert.deepEqual(paths, ["/release", "/release"]);
  resolvers[0]("browse");
  resolvers[1]("preflight");
  assert.equal(await browse, "browse");
  assert.equal(await preflight, "preflight");
  assert.equal(await sharedPreflight, "preflight");
});

test("omitted, empty, and noCache:false options share one default-key request", async () => {
  const calls = [];
  const inspect = createInspectionCoordinator((path, options) => {
    calls.push({ path, options });
    return Promise.resolve("manifest");
  });

  const results = await Promise.all([
    inspect("/release"),
    inspect("/release", { options: {} }),
    inspect("/release", { options: { noCache: false } }),
  ]);

  assert.deepEqual(results, ["manifest", "manifest", "manifest"]);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].path, "/release");
  assert.equal(calls[0].options, undefined);
});

test("noCache splits the key: a cache-bypassing request never shares a cached one", async () => {
  const calls = [];
  const pending = [];
  const inspect = createInspectionCoordinator((path, options) => new Promise(resolve => {
    calls.push({ path, options });
    pending.push(resolve);
  }));

  const cached = inspect("/release");
  const bypass = inspect("/release", { options: { noCache: true } });
  const sharedBypass = inspect("/release", { options: { noCache: true } });
  await Promise.resolve();

  assert.deepEqual(calls.map(call => call.options), [undefined, { noCache: true }]);
  pending[0]("cached");
  pending[1]("bypass");
  assert.equal(await cached, "cached");
  assert.equal(await bypass, "bypass");
  assert.equal(await sharedBypass, "bypass");
});

test("an older same-key inspection settling first cannot evict the newer forced request", async () => {
  const resolvers = [];
  const calls = [];
  const inspect = createInspectionCoordinator((path, options) => new Promise(resolve => {
    calls.push({ path, options });
    resolvers.push(resolve);
  }));

  const first = inspect("/release", { options: { noCache: true } });
  const forced = inspect("/release", { force: true, options: { noCache: true } });
  await Promise.resolve();

  assert.equal(calls.length, 2);

  resolvers[0]("first");
  assert.equal(await first, "first");

  const third = inspect("/release", { options: { noCache: true } });
  await Promise.resolve();
  assert.equal(calls.length, 2, "the third call must reuse the pending forced request");

  resolvers[1]("forced");
  assert.equal(await forced, "forced");
  assert.equal(await third, "forced");
});
