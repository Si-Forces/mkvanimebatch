import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import vm from "node:vm";

const osDetectScript = fs.readFileSync(
  path.join(import.meta.dirname, "os-detect.js"),
  "utf8"
);

function runDetector({ userAgent = "", platform = "" } = {}) {
  const classList = new Set();
  const context = {
    navigator: {
      userAgent,
      platform,
    },
    document: {
      documentElement: {
        classList: {
          add(cls) {
            classList.add(cls);
          },
          contains(cls) {
            return classList.has(cls);
          },
        },
      },
    },
  };
  vm.runInNewContext(osDetectScript, context);
  return classList;
}

test("os-detect: adds is-windows when userAgent or platform contains win", () => {
  const classesFromUa = runDetector({ userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" });
  assert.equal(classesFromUa.has("is-windows"), true);
  assert.equal(classesFromUa.has("is-mac"), false);
  assert.equal(classesFromUa.has("is-linux"), false);

  const classesFromPlatform = runDetector({ platform: "Win32" });
  assert.equal(classesFromPlatform.has("is-windows"), true);
});

test("os-detect: adds is-mac when userAgent or platform contains mac", () => {
  const classesFromUa = runDetector({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" });
  assert.equal(classesFromUa.has("is-mac"), true);
  assert.equal(classesFromUa.has("is-windows"), false);
  assert.equal(classesFromUa.has("is-linux"), false);

  const classesFromPlatform = runDetector({ platform: "MacIntel" });
  assert.equal(classesFromPlatform.has("is-mac"), true);
});

test("os-detect: adds is-linux when userAgent or platform contains linux", () => {
  const classesFromUa = runDetector({ userAgent: "Mozilla/5.0 (X11; Linux x86_64)" });
  assert.equal(classesFromUa.has("is-linux"), true);
  assert.equal(classesFromUa.has("is-windows"), false);
  assert.equal(classesFromUa.has("is-mac"), false);

  const classesFromPlatform = runDetector({ platform: "Linux x86_64" });
  assert.equal(classesFromPlatform.has("is-linux"), true);
});

test("os-detect: does not add any class when OS is unknown or platform is undefined", () => {
  const classes = runDetector({ userAgent: "UnknownDevice/1.0", platform: undefined });
  assert.equal(classes.size, 0);
});
