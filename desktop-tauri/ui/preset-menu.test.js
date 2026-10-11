import assert from "node:assert/strict";
import test from "node:test";
import { createPresetMenu } from "./preset-menu.js";

const FAST = { id: "fast", name: "Fast muxing", bitrate: "128k", complexity: 3 };
const CUSTOM = { id: "custom", name: "Custom", bitrate: "192k", complexity: 5 };

/** Just enough of an element for the rows `load()` builds. */
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
 * A page with the sidebar's empty `#preset-list` and the bitrate and complexity fields, which
 * start on the 192k / 5 defaults. `profiles` is what `get_preset_profiles` returns, or an `Error`
 * instance to throw. `commands` lists every command invoked: `load` logs a failed call rather
 * than throwing, so a wrong command shows up here, not as a failed assertion inside `invoke`.
 */
function presetHarness(profiles) {
  const commands = [];
  const list = fakeElement("section");
  const rows = () => list.children.filter(row => !row.removed);
  list.querySelectorAll = selector => (selector === ".preset-row" ? rows() : []);
  const elements = new Map([
    ["preset-list", list],
    ["remux-bitrate", { value: "192k" }],
    ["remux-complexity", { value: "5" }],
    ["remux-cplx-val", { value: "5" }],
  ]);
  const doc = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: selector => (selector === ".profile-option-item" ? rows() : []),
    createElement: fakeElement,
  };
  const invoke = async command => {
    commands.push(command);
    if (profiles instanceof Error) throw profiles;
    return profiles;
  };
  const menu = createPresetMenu({ invoke, doc });
  const field = id => elements.get(id).value;
  const activeRows = () => rows().filter(row => row.classes.has("active")).map(row => row.dataset.preset);
  return { menu, commands, rows, field, activeRows };
}

test("load renders one row per catalog profile, from the catalog alone", async () => {
  const page = presetHarness([FAST, CUSTOM]);
  await page.menu.load();
  assert.deepEqual(page.commands, ["get_preset_profiles"]);
  assert.deepEqual(page.rows().map(row => row.dataset.preset), ["fast", "custom"]);
  assert.deepEqual(page.rows().map(row => row.textContent), [
    "Fast muxingOpus 128k · complexity 3",
    "CustomOpus 192k · complexity 5",
  ]);
  // The custom row's subtitle is the one app.js keeps on the form's values.
  assert.equal(page.rows()[1].children[1].children[1].id, "preset-custom-sub");
  // Rows freeze with the rest of the job-locked controls.
  assert.ok(page.rows().every(row => row.className.includes("lock-while-job")));

  // A second load replaces the rows rather than doubling them.
  await page.menu.load();
  assert.equal(page.rows().length, 2);
});

test("the menu applies only what get_preset_profiles delivered", async () => {
  const page = presetHarness([FAST]);
  await page.menu.load();

  // "hifi" is not in the catalog, and the page carries no copy of it to fall back on.
  page.menu.apply("hifi");
  assert.equal(page.field("remux-bitrate"), "192k");
  assert.equal(page.field("remux-complexity"), "5");
  assert.deepEqual(page.activeRows(), []);

  page.menu.apply("fast");
  assert.equal(page.field("remux-bitrate"), "128k");
  assert.equal(page.field("remux-complexity"), "3");
  assert.equal(page.field("remux-cplx-val"), "3");
  assert.deepEqual(page.activeRows(), ["fast"]);
  assert.deepEqual(page.rows().map(row => row.attributes["aria-pressed"]), ["true"]);
});

test("custom marks the form as hand-edited and leaves its fields alone, even before the catalog loads", async () => {
  const page = presetHarness([FAST, CUSTOM]);
  page.menu.apply("custom");
  assert.equal(page.field("remux-bitrate"), "192k");
  assert.equal(page.field("remux-complexity"), "5");
  // Rows rendered after the choice still show it.
  await page.menu.load();
  assert.deepEqual(page.activeRows(), ["custom"]);
});

test("a failed get_preset_profiles leaves the catalog and the rows empty and logs the failure", async t => {
  const logged = t.mock.method(console, "error", () => {});
  const page = presetHarness(new Error("unavailable"));
  await page.menu.load();
  assert.deepEqual(page.commands, ["get_preset_profiles"]);
  assert.equal(page.rows().length, 0);
  page.menu.apply("fast");
  assert.equal(page.field("remux-bitrate"), "192k");
  assert.equal(logged.mock.calls.some(({ arguments: [label] }) => label === "[get_preset_profiles] failed:"), true);
});
