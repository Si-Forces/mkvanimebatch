import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import zlib from "node:zlib";

// The bundle icons are derived from src-tauri/icons/icon.svg, and nothing re-renders them
// automatically. These tests check each derived file is the format its extension promises (a
// renamed PNG passes for an icon.icns unnoticed) and catch a file drifting from its source: the
// containers must embed the standalone PNGs, icon.png must hold the glyph where icon.svg puts it,
// and icon.svg must draw the same glyph as the title bar.
const icons = new URL("../src-tauri/icons/", import.meta.url);
const read = name => fs.readFileSync(new URL(name, icons));
const PNG_MAGIC = Buffer.from([0x89, 0x50, 0x4e, 0x47]);

// ICNS: an 'icns' header holding the file length, then entries of OSType, length, payload.
const icnsEntries = icns => {
  assert.equal(icns.toString("ascii", 0, 4), "icns");
  assert.equal(icns.readUInt32BE(4), icns.length);
  const entries = new Map();
  for (let at = 8; at < icns.length; ) {
    const type = icns.toString("ascii", at, at + 4), length = icns.readUInt32BE(at + 4);
    assert.ok(length > 8, `entry ${type} has a length past its header`);
    entries.set(type, icns.subarray(at + 8, at + length));
    at += length;
  }
  return entries;
};

// ICO: a 6-byte header, then 16-byte directory entries (width 0 means 256) pointing at the images.
const icoEntries = ico => {
  assert.deepEqual([ico.readUInt16LE(0), ico.readUInt16LE(2)], [0, 1]);
  const entries = new Map();
  for (let i = 0; i < ico.readUInt16LE(4); i++) {
    const dir = 6 + 16 * i, size = ico.readUInt32LE(dir + 8), offset = ico.readUInt32LE(dir + 12);
    entries.set(ico[dir] || 256, ico.subarray(offset, offset + size));
  }
  return entries;
};

test("icon.icns is an ICNS container of PNG entries", () => {
  for (const [type, payload] of icnsEntries(read("icon.icns"))) {
    assert.ok(payload.subarray(0, 4).equals(PNG_MAGIC), `entry ${type} holds a PNG`);
  }
});

test("icon.ico is an ICO whose entries are PNG-compressed", () => {
  const entries = icoEntries(read("icon.ico"));
  assert.ok(entries.size > 0);
  for (const [size, image] of entries) {
    assert.ok(image.subarray(0, 4).equals(PNG_MAGIC), `the ${size}px entry holds a PNG`);
  }
});

test("the PNG icons are PNGs", () => {
  for (const name of ["32x32.png", "128x128.png", "128x128@2x.png", "icon.png"]) {
    assert.ok(read(name).subarray(0, 4).equals(PNG_MAGIC), name);
  }
});

test("icon.icns and icon.ico embed the standalone PNGs byte for byte", () => {
  const icns = icnsEntries(read("icon.icns"));
  assert.deepEqual([...icns.keys()], ["ic07", "ic08", "ic09"]);
  assert.ok(icns.get("ic07").equals(read("128x128.png")), "ic07 is 128x128.png");
  assert.ok(icns.get("ic08").equals(read("128x128@2x.png")), "ic08 is 128x128@2x.png");
  assert.ok(icns.get("ic09").equals(read("icon.png")), "ic09 is icon.png");
  const ico = icoEntries(read("icon.ico"));
  assert.deepEqual([...ico.keys()], [16, 24, 32, 48, 64, 256]);
  assert.ok(ico.get(32).equals(read("32x32.png")), "the 32px ICO entry is 32x32.png");
  assert.ok(ico.get(256).equals(read("128x128@2x.png")), "the 256px ICO entry is 128x128@2x.png");
});

// Decodes an 8-bit RGBA, non-interlaced PNG (what the icons are rendered as) and returns its alpha.
const pngAlpha = png => {
  const width = png.readUInt32BE(16), height = png.readUInt32BE(20);
  assert.deepEqual([png[24], png[25], png[28]], [8, 6, 0], "8-bit RGBA, not interlaced");
  const idat = [];
  for (let at = 8; at < png.length; at += 12 + png.readUInt32BE(at)) {
    if (png.toString("ascii", at + 4, at + 8) === "IDAT") idat.push(png.subarray(at + 8, at + 8 + png.readUInt32BE(at)));
  }
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * 4, px = Buffer.alloc(stride * height);
  assert.equal(raw.length, (stride + 1) * height, "one filter byte plus one row per scanline");
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    assert.ok(filter <= 4, `row ${y} has a known filter type`);
    for (let x = 0; x < stride; x++) {
      // a: the same channel one pixel left, b: one row up, c: up and left (PNG spec, section 9).
      const a = x >= 4 ? px[y * stride + x - 4] : 0;
      const b = y > 0 ? px[(y - 1) * stride + x] : 0;
      const c = x >= 4 && y > 0 ? px[(y - 1) * stride + x - 4] : 0;
      let predictor;
      switch (filter) {
        case 0: predictor = 0; break;
        case 1: predictor = a; break;
        case 2: predictor = b; break;
        case 3: predictor = (a + b) >> 1; break;
        case 4: {
          const p = a + b - c, pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
          predictor = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
        }
      }
      px[y * stride + x] = (raw[y * (stride + 1) + 1 + x] + predictor) & 0xff;
    }
  }
  return { width, height, alpha: (x, y) => px[(y * width + x) * 4 + 3] };
};

test("icon.png holds the glyph where icon.svg places it, on a transparent background", () => {
  const { width, height, alpha } = pngAlpha(read("icon.png"));
  assert.deepEqual([width, height], [512, 512]);
  let [left, top, right, bottom] = [width, height, -1, -1];
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      if (alpha(x, y) > 127) [left, top, right, bottom] = [Math.min(left, x), Math.min(top, y), Math.max(right, x), Math.max(bottom, y)];
    }
  }
  // translate(-16 -9.5) scale(44) maps the glyph to x 72..952, y 100.5..923.3 of 1024. Halved, the
  // opaque pixels run from 36,50 to 475,461 inclusive; 2px absorbs a different resampler.
  const expected = [36, 50, 475, 461];
  [left, top, right, bottom].forEach((edge, i) => assert.ok(Math.abs(edge - expected[i]) <= 2, `bbox edge ${i}: ${edge} vs ${expected[i]}`));
  assert.equal(alpha(0, 0), 0, "the corner is transparent: no tile behind the glyph");
});

test("icon.svg draws the same glyph as the title-bar brand mark", () => {
  const svg = fs.readFileSync(new URL("icon.svg", icons), "utf8");
  const html = fs.readFileSync(new URL("index.html", import.meta.url), "utf8");
  const brand = html.match(/<span class="brand-mark"[\s\S]*?<\/span>/)?.[0];
  assert.ok(brand, "index.html has a .brand-mark");
  // Every drawing element and the gradient, with the gradient id (screen vs mk-screen) normalized.
  const glyph = text =>
    [...text.matchAll(/<(?:rect|path|circle|ellipse|line|polyline|polygon|linearGradient|radialGradient|stop)\b[^>]*>/g)]
      .map(m => m[0].replace(/\bid="[\w-]+"/, 'id="g"').replace(/url\(#[\w-]+\)/, "url(#g)"));
  assert.ok(glyph(brand).length > 0, "the brand mark draws something");
  assert.deepEqual(glyph(svg), glyph(brand));
});
