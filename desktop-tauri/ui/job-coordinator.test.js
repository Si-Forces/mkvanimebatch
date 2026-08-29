import assert from "node:assert/strict";
import test from "node:test";

import { createJobCoordinator } from "./job-coordinator.js";

test("coordinator permits only one active pipeline", () => {
  const coordinator = createJobCoordinator();

  assert.equal(coordinator.begin("remux"), true);
  assert.equal(coordinator.begin("extract"), false);
  assert.equal(coordinator.activeMode, "remux");

  coordinator.finish();
  assert.equal(coordinator.begin("extract"), true);
});

test("coordinator synchronizes registered controls through lifecycle changes", () => {
  let updates = 0;
  const coordinator = createJobCoordinator();
  coordinator.register({ syncGlobalControls: () => updates++ });

  coordinator.begin("remux");
  coordinator.isStopping = true;
  coordinator.syncControls();
  coordinator.finish();

  assert.equal(updates, 3);
  assert.equal(coordinator.activeMode, null);
  assert.equal(coordinator.isStopping, false);
});
