import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import vm from "node:vm";

const themeInitScript = fs.readFileSync(
  path.join(import.meta.dirname, "theme-init.js"),
  "utf8"
);

function runThemeInit({ storedTheme = null, prefersDark = true, storageThrows = false } = {}) {
  const dataset = {};
  const context = {
    localStorage: {
      getItem(key) {
        if (storageThrows) throw new Error("Storage blocked");
        return key === "mkv-theme" ? storedTheme : null;
      },
    },
    window: {
      matchMedia(query) {
        return {
          matches: query.includes("dark") ? prefersDark : !prefersDark,
        };
      },
    },
    document: {
      documentElement: {
        dataset,
      },
    },
  };
  vm.runInNewContext(themeInitScript, context);
  return dataset.theme;
}

test("theme-init: honours explicit stored light theme", () => {
  assert.equal(runThemeInit({ storedTheme: "light", prefersDark: true }), "light");
});

test("theme-init: honours explicit stored dark theme", () => {
  assert.equal(runThemeInit({ storedTheme: "dark", prefersDark: false }), "dark");
});

test("theme-init: follows OS preference when no theme is stored", () => {
  assert.equal(runThemeInit({ storedTheme: null, prefersDark: false }), "light");
  assert.equal(runThemeInit({ storedTheme: null, prefersDark: true }), "dark");
});

test("theme-init: ignores an unrecognized stored theme", () => {
  assert.equal(runThemeInit({ storedTheme: "blue", prefersDark: false }), "light");
  assert.equal(runThemeInit({ storedTheme: "blue", prefersDark: true }), "dark");
});

test("theme-init: handles localStorage error and falls back to OS preference", () => {
  assert.equal(runThemeInit({ storageThrows: true, prefersDark: false }), "light");
  assert.equal(runThemeInit({ storageThrows: true, prefersDark: true }), "dark");
});
