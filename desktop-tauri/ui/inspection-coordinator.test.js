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

  resolvers[0]({ version: 1, items: [] });
  assert.deepEqual(await first, await shared);

  const replacement = inspect("/release");
  await Promise.resolve();
  assert.deepEqual(paths, ["/release", "/release"]);
  resolvers[1]({ version: 1, items: [{ id: "01.mkv" }] });
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
