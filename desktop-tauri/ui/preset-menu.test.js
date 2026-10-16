import assert from "node:assert/strict";
import test from "node:test";
import { createPresetMenu } from "./preset-menu.js";
import { draft } from "./fake-core.js";

const FAST = { id: "fast", name: "Fast muxing", bitrate: "128k", complexity: 3 };
const CUSTOM = { id: "custom", name: "Custom", bitrate: "192k", complexity: 5 };

/** Just enough of an element for the rows `render()` builds. */
function fakeElement(tag) {
  const el = {
    tag,
    dataset: {},
    attributes: {},
    children: [],
    classes: new Set(),
    textContent: "",
    setAttribute: (name, value) => { el.attributes[name] = value; },
    append: (...nodes) => {
      for (const node of nodes) {
        el.children.push(node);
        el.textContent += typeof node === "string" ? node : node.textContent;
      }
    },
    appendChild: node => el.append(node),
    remove: () => { el.removed = true; },
  };
  el.classList = { toggle: (name, on) => (on ? el.classes.add(name) : el.classes.delete(name)) };
  return el;
}

/**
 * A page with the sidebar's empty `#preset-list`. `intents` lists what a click dispatched;
 * `state.locked` is what `isLocked()` reports.
 */
function presetHarness() {
  const intents = [];
  const state = { locked: false };
  const list = fakeElement("section");
  const handlers = new Map();
  list.addEventListener = (name, handler) => handlers.set(name, handler);
  const rows = () => list.children.filter(row => !row.removed);
  list.querySelectorAll = selector => (selector === ".preset-row" ? rows() : []);
  const findSub = () => rows().map(row => row.children[1].children[1]).find(sub => sub.id === "preset-custom-sub");
  const doc = {
    getElementById: id => (id === "preset-list" ? list : id === "preset-custom-sub" ? findSub() ?? null : null),
    querySelectorAll: selector => (selector === ".profile-option-item" ? rows() : []),
    createElement: fakeElement,
  };
  const dispatch = async intent => { intents.push(intent); return true; };
  const menu = createPresetMenu({ doc, dispatch, isLocked: () => state.locked });
  menu.bind();
  const activeRows = () => rows().filter(row => row.classes.has("active")).map(row => row.dataset.preset);
  const click = row => handlers.get("click")({
    target: { closest: selector => (selector === ".profile-option-item[data-preset]" ? row : null) },
  });
  return { menu, intents, state, rows, activeRows, click, findSub };
}

test("render draws one row per published profile and marks the draft's preset active", () => {
  const page = presetHarness();
  assert.equal(page.menu.render([FAST, CUSTOM], draft("remux", { preset: "fast" })), true);
  assert.deepEqual(page.rows().map(row => row.dataset.preset), ["fast", "custom"]);
  assert.deepEqual(page.rows().map(row => row.textContent), [
    "Fast muxingOpus 128k · complexity 3",
    // The custom row's subtitle shows the draft's own encode, not the catalog's.
    "CustomOpus 192k · complexity 5",
  ]);
  assert.deepEqual(page.activeRows(), ["fast"]);
  assert.deepEqual(page.rows().map(row => row.attributes["aria-pressed"]), ["true", "false"]);
  // Rows freeze with the rest of the job-locked controls.
  assert.ok(page.rows().every(row => row.className.includes("lock-while-job")));

  // The same catalog again keeps the rows and follows the draft.
  assert.equal(page.menu.render([FAST, CUSTOM], draft("remux", { preset: "custom", bitrate: "256k", complexity: "8" })), false);
  assert.equal(page.rows().length, 2);
  assert.deepEqual(page.activeRows(), ["custom"]);
  assert.equal(page.findSub().textContent, "Opus 256k · complexity 8");
});

test("an empty catalog renders no rows", () => {
  const page = presetHarness();
  page.menu.render([], draft("remux"));
  assert.equal(page.rows().length, 0);
});

test("a click sends set_preset for its row, and nothing while a job holds the form", async () => {
  const page = presetHarness();
  page.menu.render([FAST, CUSTOM], draft("remux"));
  assert.equal(await page.click(page.rows()[0]), true);
  assert.deepEqual(page.intents, [{ type: "set_preset", mode: "remux", preset: "fast" }]);
  // The click changes nothing itself: the published draft does.
  assert.deepEqual(page.activeRows(), []);

  page.state.locked = true;
  page.click(page.rows()[1]);
  page.click(null);
  assert.equal(page.intents.length, 1);
});
