import assert from "node:assert/strict";
import test from "node:test";
import { createSettingsDraft } from "./settings-draft.js";
import { draft } from "./fake-core.js";

/** A form field: `type` "checkbox" holds `checked`, any other a `value`. */
function field(type = "text", value = "") {
  const handlers = new Map();
  return {
    type,
    value,
    checked: false,
    handlers,
    addEventListener: (name, handler) => handlers.set(name, handler),
  };
}

/**
 * The remux sheet's fields and the extract input folder. `answers` holds each dispatch until the
 * test settles it, in the order sent.
 */
function draftHarness() {
  const fields = new Map([
    ["remux-input-dir", field()],
    ["remux-out-dir", field()],
    ["remux-bitrate", field("select-one", "192k")],
    ["remux-complexity", field("range", "5")],
    ["remux-cplx-val", field("number", "5")],
    ["remux-dry-run", field("checkbox")],
    ["extract-input-dir", field()],
  ]);
  const doc = { activeElement: null, getElementById: id => fields.get(id) ?? null };
  const intents = [];
  const answers = [];
  const dispatch = intent => {
    intents.push(intent);
    return new Promise(resolve => answers.push(resolve));
  };
  const form = createSettingsDraft({ doc, dispatch });
  form.bind();
  return { form, fields, doc, intents, answers, get: id => fields.get(id) };
}

const flush = () => new Promise(resolve => setImmediate(resolve));

test("every input and change of a field sends its value as the mode's draft key", () => {
  const page = draftHarness();
  page.get("remux-input-dir").value = "/anime";
  page.get("remux-input-dir").handlers.get("input")();
  page.get("remux-cplx-val").value = "7";
  page.get("remux-cplx-val").handlers.get("change")();
  page.get("remux-dry-run").checked = true;
  page.get("remux-dry-run").handlers.get("change")();
  page.get("extract-input-dir").value = "/other";
  page.get("extract-input-dir").handlers.get("input")();
  assert.deepEqual(page.intents, [
    { type: "edit_draft", mode: "remux", changes: { input_dir: "/anime" } },
    { type: "edit_draft", mode: "remux", changes: { complexity: "7" } },
    { type: "edit_draft", mode: "remux", changes: { dry_run: true } },
    { type: "edit_draft", mode: "extract", changes: { input_dir: "/other" } },
  ]);
});

test("a published draft is written into the unfocused fields, and never into the focused one", () => {
  const page = draftHarness();
  page.doc.activeElement = page.get("remux-out-dir");
  page.get("remux-out-dir").value = "/out/typ";
  const written = page.form.apply("remux", draft("remux", {
    input_dir: "/anime",
    out_dir: "/out/ty",
    complexity: "9",
    dry_run: true,
  }));
  assert.equal(page.get("remux-input-dir").value, "/anime");
  // The caret stays where the user types.
  assert.equal(page.get("remux-out-dir").value, "/out/typ");
  assert.equal(page.get("remux-complexity").value, "9");
  assert.equal(page.get("remux-cplx-val").value, "9");
  assert.equal(page.get("remux-dry-run").checked, true);
  // The extract draft has no complexity, and its fields are the extract ones.
  assert.deepEqual([...written].sort(), ["complexity", "dry_run", "input_dir"]);
  assert.equal(page.get("extract-input-dir").value, "");

  // Once the field loses focus, the published value is written.
  page.doc.activeElement = null;
  page.form.apply("remux", draft("remux", { out_dir: "/out/typed" }));
  assert.equal(page.get("remux-out-dir").value, "/out/typed");
});

test("a key whose edit is still on its way is not written back until the core answered it", async () => {
  const page = draftHarness();
  page.get("remux-out-dir").value = "/new";
  page.get("remux-out-dir").handlers.get("change")();
  // A publication from before the core applied the edit holds the old value.
  page.form.apply("remux", draft("remux", { out_dir: "/old", input_dir: "/anime" }));
  assert.equal(page.get("remux-out-dir").value, "/new");
  assert.equal(page.get("remux-input-dir").value, "/anime");

  page.answers.shift()(true);
  await flush();
  page.form.apply("remux", draft("remux", { out_dir: "/new2" }));
  assert.equal(page.get("remux-out-dir").value, "/new2");
});

test("the complexity slider takes only a value that parses, so a cleared box does not move it", () => {
  const page = draftHarness();
  page.form.apply("remux", draft("remux", { complexity: "" }));
  assert.equal(page.get("remux-cplx-val").value, "");
  assert.equal(page.get("remux-complexity").value, "5");
  page.form.apply("remux", draft("remux", { complexity: "8x" }));
  assert.equal(page.get("remux-complexity").value, "8");
});
