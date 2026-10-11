import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";

// The bundle icons are derived from src-tauri/icons/icon.svg. This checks each derived file is the
// format its extension promises: an icon.icns that was really a renamed PNG shipped unnoticed once.
const icons = new URL("../src-tauri/icons/", import.meta.url);
const read = name => fs.readFileSync(new URL(name, icons));
const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47]);

test("icon.icns is an ICNS container of PNG entries", () => {
  const icns = read("icon.icns");
  assert.equal(icns.toString("ascii", 0, 4), "icns");
  assert.equal(icns.readUInt32BE(4), icns.length);
  for (let at = 8; at < icns.length; at += icns.readUInt32BE(at + 4)) {
    assert.ok(icns.readUInt32BE(at + 4) > 8, "an entry has a length past its header");
    assert.ok(icns.subarray(at + 8, at + 12).equals(PNG_MAGIC), `entry ${icns.toString("ascii", at, at + 4)} holds a PNG`);
  }
});

test("icon.ico is an ICO whose entries are PNG-compressed", () => {
  const ico = read("icon.ico");
  assert.deepEqual([ico.readUInt16LE(0), ico.readUInt16LE(2)], [0, 1]);
  const count = ico.readUInt16LE(4);
  assert.ok(count > 0);
  for (let i = 0; i < count; i++) {
    const offset = ico.readUInt32LE(6 + 16 * i + 12);
    assert.ok(ico.subarray(offset, offset + 4).equals(PNG_MAGIC), `entry ${i} holds a PNG`);
  }
});

test("the PNG icons are PNGs", () => {
  for (const name of ["32x32.png", "128x128.png", "128x128@2x.png", "icon.png"]) {
    assert.ok(read(name).subarray(0, 4).equals(PNG_MAGIC), name);
  }
});
