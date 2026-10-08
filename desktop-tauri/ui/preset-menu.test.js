import assert from "node:assert/strict";
import test from "node:test";
import { createPresetMenu } from "./preset-menu.js";

const FAST = { id: "fast", icon: "⚡", name: "Fast Muxing (128k)", bitrate: "128k", complexity: 3 };

/** A menu option that records the state `setActiveProfileOption` gives it. */
function menuOption(preset) {
  const option = { dataset: { preset }, active: false, attributes: {} };
  option.classList = { toggle: (name, on) => { if (name === "active") option.active = on; } };
  option.setAttribute = (name, value) => { option.attributes[name] = value; };
  return option;
}

/**
 * A page with the titlebar's profile picker (name, button, and a "fast", "hifi" and "custom"
 * option) and the bitrate and complexity fields, which start on the 192k / 5 defaults.
 * `profiles` is what `get_preset_profiles` returns, or an `Error` instance to throw. `commands`
 * lists every command invoked: `load` logs a failed call rather than throwing, so a wrong command
 * shows up here, not as a failed assertion inside `invoke`.
 */
function presetHarness(profiles) {
  const commands = [];
  const elements = new Map([
    ["active-profile-name", {}],
    ["active-profile-btn", { dataset: {} }],
    ["remux-bitrate", { value: "192k" }],
    ["remux-complexity", { value: "5" }],
    ["remux-cplx-val", { value: "5" }],
  ]);
  const options = ["fast", "hifi", "custom"].map(menuOption);
  const doc = {
    getElementById: id => elements.get(id) ?? null,
    querySelectorAll: selector => (selector === ".profile-option-item" ? options : []),
  };
  const invoke = async command => {
    commands.push(command);
    if (profiles instanceof Error) throw profiles;
    return profiles;
  };
  const menu = createPresetMenu({ invoke, doc });
  const field = id => elements.get(id).value;
  const activeOptions = () => options.filter(option => option.active).map(option => option.dataset.preset);
  return {
    menu,
    commands,
    options,
    field,
    activeOptions,
    name: () => elements.get("active-profile-name").textContent,
    button: () => elements.get("active-profile-btn").dataset.preset,
  };
}

test("the menu applies only what get_preset_profiles delivered", async () => {
  const page = presetHarness([FAST]);
  await page.menu.load();
  assert.deepEqual(page.commands, ["get_preset_profiles"]);

  // "hifi" is not in the catalog, and the page carries no copy of it to fall back on.
  page.menu.apply("hifi");
  assert.equal(page.field("remux-bitrate"), "192k");
  assert.equal(page.field("remux-complexity"), "5");
  assert.equal(page.name(), undefined);
  assert.equal(page.button(), undefined);
  assert.deepEqual(page.activeOptions(), []);

  page.menu.apply("fast");
  assert.equal(page.field("remux-bitrate"), "128k");
  assert.equal(page.field("remux-complexity"), "3");
  assert.equal(page.field("remux-cplx-val"), "3");
  assert.equal(page.name(), "⚡ Fast Muxing (128k)");
  assert.equal(page.button(), "fast");
  assert.deepEqual(page.activeOptions(), ["fast"]);
  assert.deepEqual(page.options.map(option => option.attributes["aria-pressed"]), ["true", "false", "false"]);
});

test("custom marks the form as hand-edited and leaves its fields alone, even before the catalog loads", () => {
  const page = presetHarness([FAST]);
  page.menu.apply("custom");
  assert.equal(page.name(), "🛠️ Custom Configuration");
  assert.equal(page.button(), "custom");
  assert.deepEqual(page.activeOptions(), ["custom"]);
  assert.equal(page.field("remux-bitrate"), "192k");
  assert.equal(page.field("remux-complexity"), "5");
});

test("a failed get_preset_profiles leaves the catalog empty and logs the failure", async t => {
  const logged = t.mock.method(console, "error", () => {});
  const page = presetHarness(new Error("unavailable"));
  await page.menu.load();
  assert.deepEqual(page.commands, ["get_preset_profiles"]);
  page.menu.apply("fast");
  assert.equal(page.field("remux-bitrate"), "192k");
  assert.equal(page.name(), undefined);
  assert.equal(logged.mock.calls.some(({ arguments: [label] }) => label === "[get_preset_profiles] failed:"), true);
});
