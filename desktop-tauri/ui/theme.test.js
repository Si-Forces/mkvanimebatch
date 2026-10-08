import assert from "node:assert/strict";
import test from "node:test";
import { createTheme } from "./theme.js";

/**
 * A page and its OS colour scheme. `published` is what `theme-init.js` left in `dataset`: nothing
 * when the bootstrap never ran. `data-theme` is the `dataset.theme` it mirrors in a real DOM.
 * `storage.setItem` throws when `storageBlocked`; every key it was asked to save is in `saved`, and
 * every key `storage.getItem` was asked for is in `reads`.
 * `schemeChange(matches)` plays an OS colour scheme change to the live listeners.
 */
function themeHarness({ published = {}, osDark = false, storageBlocked = false } = {}) {
  const dataset = { ...published };
  const documentElement = {
    dataset,
    setAttribute: (name, value) => {
      assert.equal(name, "data-theme");
      dataset.theme = value;
    },
    getAttribute: name => {
      assert.equal(name, "data-theme");
      return dataset.theme ?? null;
    },
  };
  const schemeListeners = [];
  const win = {
    matchMedia: query => {
      assert.equal(query, "(prefers-color-scheme: dark)");
      return { matches: osDark, addEventListener: (name, handler) => schemeListeners.push([name, handler]) };
    },
  };
  const saved = [];
  const reads = [];
  const storage = {
    // theme.js must not read the choice back: `theme-init.js` is its only reader.
    getItem: key => { reads.push(key); return "dark"; },
    setItem: (key, value) => {
      if (storageBlocked) throw new Error("storage blocked");
      saved.push([key, value]);
    },
  };
  const pinned = [];
  const invoke = async (command, args) => {
    assert.equal(command, "set_window_theme");
    pinned.push(args.theme);
  };
  const theme = createTheme({ doc: { documentElement }, win, storage, invoke });
  return {
    theme,
    dataset,
    saved,
    reads,
    pinned,
    current: () => dataset.theme,
    schemeChange: matches => {
      for (const [name, handler] of schemeListeners) {
        assert.equal(name, "change");
        handler({ matches });
      }
    },
    listening: () => schemeListeners.length,
  };
}

test("an explicit toggle survives a failed storage write when the OS scheme changes", () => {
  const page = themeHarness({ osDark: false, storageBlocked: true });
  page.theme.init();
  assert.equal(page.current(), "light");

  page.theme.toggle();
  assert.equal(page.current(), "dark");
  // The DOM is the single source of truth after a toggle: an explicit choice must be
  // published to dataset.themeExplicit, or storage and the dataset disagree again.
  assert.equal(page.dataset.themeExplicit, "true");

  page.schemeChange(false);
  assert.equal(page.current(), "dark");
});

test("a toggle flips the theme, saves it under mkv-theme and pins the native window", () => {
  const page = themeHarness({ published: { theme: "dark", themeExplicit: "false" } });
  page.theme.init();

  page.theme.toggle();
  assert.equal(page.current(), "light");
  assert.deepEqual(page.saved, [["mkv-theme", "light"]]);
  assert.deepEqual(page.pinned, ["light"]);

  page.theme.toggle();
  assert.equal(page.current(), "dark");
  assert.deepEqual(page.saved, [["mkv-theme", "light"], ["mkv-theme", "dark"]]);
  assert.deepEqual(page.pinned, ["light", "dark"]);
});

test("a stored choice published by theme-init.js is applied and pinned, and the OS scheme no longer moves it", () => {
  const page = themeHarness({ published: { theme: "light", themeExplicit: "true" }, osDark: true });
  page.theme.init();
  assert.equal(page.current(), "light");
  assert.deepEqual(page.pinned, ["light"]);
  assert.equal(page.listening(), 0, "an explicit choice has no OS scheme to follow");
});

test("a theme published as following the OS is left unpinned and follows the OS live", () => {
  const page = themeHarness({ published: { theme: "dark", themeExplicit: "false" }, osDark: true });
  page.theme.init();
  assert.deepEqual(page.pinned, []);
  assert.equal(page.current(), "dark");

  page.schemeChange(false);
  assert.equal(page.current(), "light");
  assert.deepEqual(page.pinned, [], "following the OS must leave the native window unpinned");
});

test("an unrecognized stored theme, which theme-init.js publishes as following the OS, follows it live", () => {
  // With a stored value that is neither "light" nor "dark" and the OS in light, this is the state
  // theme-init.js leaves behind.
  const page = themeHarness({ published: { theme: "light", themeExplicit: "false" }, osDark: false });
  page.theme.init();
  assert.equal(page.current(), "light");
  assert.deepEqual(page.pinned, []);

  page.schemeChange(true);
  assert.equal(page.current(), "dark");
  assert.deepEqual(page.pinned, []);
});

test("an explicit publication that is neither light nor dark is not applied or pinned", () => {
  const page = themeHarness({ published: { theme: "blue", themeExplicit: "true" }, osDark: false });
  page.theme.init();
  assert.deepEqual(page.pinned, []);

  page.schemeChange(true);
  assert.equal(page.current(), "dark");
  assert.deepEqual(page.pinned, []);
});

test("theme.js never reads the stored theme back, even when the bootstrap published nothing", t => {
  // theme-init.js is the only resolver of the stored choice. Neither the injected storage nor the
  // global `localStorage` may be read.
  const globalReads = [];
  const prior = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: { getItem: key => { globalReads.push(key); return "dark"; }, setItem() {} },
  });
  t.after(() => {
    if (prior) Object.defineProperty(globalThis, "localStorage", prior);
    else delete globalThis.localStorage;
  });

  // Nothing published: the bootstrap never ran, so only the live OS scheme may decide.
  const page = themeHarness({ published: {}, osDark: false });
  page.theme.init();
  assert.equal(page.current(), "light", "without a published theme the OS scheme wins");
  assert.deepEqual(page.pinned, [], "following the OS must leave the native window unpinned");

  page.schemeChange(true);
  assert.equal(page.current(), "dark");
  page.theme.toggle();
  assert.deepEqual(page.reads, []);
  assert.deepEqual(globalReads, []);
});
