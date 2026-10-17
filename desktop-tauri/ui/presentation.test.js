// The presentation rules the view used to hold are the Desktop Core's now, tested in
// `crates/mkv-desktop-core/src/presentation.rs`; only the sparkline drawing stays here.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import { sparkPoints } from "./presentation.js";

test("sparkline points scale to the box and clamp out-of-range samples", () => {
  assert.equal(sparkPoints([5], 100), "");
  assert.equal(sparkPoints([0, 100, 200], 100, 64, 20), "0.0,18.0 32.0,2.0 64.0,2.0");
});

// WebKitGTK before 2.40 cannot parse a regex lookbehind, and the SyntaxError stops the whole
// module graph from loading, so the window stays empty. The deb and rpm use the host's WebKitGTK.
test("no UI module uses a regex lookbehind", () => {
  const dir = new URL(".", import.meta.url);
  for (const file of fs.readdirSync(dir).filter(name => name.endsWith(".js") && !name.endsWith(".test.js"))) {
    assert.doesNotMatch(fs.readFileSync(new URL(file, dir), "utf8"), /\(\?<[=!]/, file);
  }
});
