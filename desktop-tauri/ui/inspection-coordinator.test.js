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

test("an older inspection cannot clear a newer request", async () => {
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

test("ordinary browse defaults produce default path key and share promises", async () => {
  const calls = [];
  const inspect = createInspectionCoordinator((path, options) => {
    calls.push({ path, options });
    return Promise.resolve("manifest");
  });

  const p1 = inspect("/release");
  const p2 = inspect("/release", { options: {} });
  const [r1, r2] = await Promise.all([p1, p2]);

  assert.equal(r1, "manifest");
  assert.equal(r2, "manifest");
  assert.equal(calls.length, 1);
  assert.equal(calls[0].path, "/release");
  assert.equal(calls[0].options, undefined);
});

test("differing options trigger separate inspect calls", async () => {
  const calls = [];
  const pending = [];
  const inspect = createInspectionCoordinator((path, options) => new Promise(resolve => {
    calls.push({ path, options });
    pending.push(resolve);
  }));

  const p1 = inspect("/release", { options: { workers: 4, ioJobs: 2 } });
  const p2 = inspect("/release", { options: { workers: 8, ioJobs: 2 } });
  const p3 = inspect("/release", { options: { workers: 4, ioJobs: 0 } });
  const p4 = inspect("/release", { options: { workers: 4, ioJobs: 2, noCache: true } });
  await Promise.resolve();

  assert.equal(calls.length, 4);
  assert.deepEqual(calls[0].options, { workers: 4, ioJobs: 2 });
  assert.deepEqual(calls[1].options, { workers: 8, ioJobs: 2 });
  assert.deepEqual(calls[2].options, { workers: 4, ioJobs: 0 });
  assert.deepEqual(calls[3].options, { workers: 4, ioJobs: 2, noCache: true });

  pending.forEach((res, i) => res(`result-${i}`));
  assert.equal(await p1, "result-0");
  assert.equal(await p2, "result-1");
  assert.equal(await p3, "result-2");
  assert.equal(await p4, "result-3");
});

test("matching options dedupe and share a single in-flight inspect", async () => {
  const calls = [];
  let resolver;
  const inspect = createInspectionCoordinator((path, options) => new Promise(resolve => {
    calls.push({ path, options });
    resolver = resolve;
  }));

  const p1 = inspect("/release", { options: { workers: 6, ioJobs: 0, noCache: true } });
  const p2 = inspect("/release", { options: { workers: 6, ioJobs: 0, noCache: true } });
  await Promise.resolve();

  assert.equal(calls.length, 1);
  resolver("done");
  assert.equal(await p1, "done");
  assert.equal(await p2, "done");
});

test("forced inspection with options cleans up properly when old resolves first, sharing new pending with third call", async () => {
  const resolvers = [];
  const calls = [];
  const inspect = createInspectionCoordinator((path, options) => new Promise(resolve => {
    calls.push({ path, options });
    resolvers.push(resolve);
  }));

  const first = inspect("/release", { options: { workers: 4 } });
  const forced = inspect("/release", { force: true, options: { workers: 4 } });
  await Promise.resolve();

  assert.equal(calls.length, 2);

  // Resolve old first
  resolvers[0]("first");
  assert.equal(await first, "first");

  // Issue third same-options call while new is pending; must reuse new promise
  const third = inspect("/release", { options: { workers: 4 } });
  await Promise.resolve();
  assert.equal(calls.length, 2);

  // Then complete
  resolvers[1]("forced");
  assert.equal(await forced, "forced");
  assert.equal(await third, "forced");
});

test("path containing ::w= regression distinguishes from no-options path under JSON key", async () => {
  const calls = [];
  const resolvers = [];
  const inspect = createInspectionCoordinator((path, options) => new Promise(resolve => {
    calls.push({ path, options });
    resolvers.push(resolve);
  }));

  const collisionPath = "/release::w=4::io=::nc=false";
  const p1 = inspect(collisionPath);
  const p2 = inspect("/release", { options: { workers: 4 } });
  await Promise.resolve();

  assert.equal(calls.length, 2);
  assert.equal(calls[0].path, collisionPath);
  assert.equal(calls[0].options, undefined);
  assert.equal(calls[1].path, "/release");
  assert.deepEqual(calls[1].options, { workers: 4 });

  resolvers[0]("collision-path");
  resolvers[1]("release-with-workers");
  assert.equal(await p1, "collision-path");
  assert.equal(await p2, "release-with-workers");
});
