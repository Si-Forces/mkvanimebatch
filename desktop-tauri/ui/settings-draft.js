/**
 * The settings sheet as a mirror of each mode's Settings Draft, which the Desktop Core holds: every
 * edit of a field sends `edit_draft` (a checkbox on change, any other field on input and change),
 * and every published draft is written back into the fields.
 * The draft keys a mode does not have are not in its draft, and their fields are not on the page.
 */

/** The fields of each draft key, as id suffixes after `${mode}-`. Complexity has a slider and a box. */
export const DRAFT_FIELDS = {
  input_dir: ["input-dir"],
  out_dir: ["out-dir"],
  fonts_dir: ["fonts-dir"],
  assets_dir: ["assets-dir"],
  bitrate: ["bitrate"],
  complexity: ["complexity", "cplx-val"],
  sub_offset: ["sub-offset"],
  workers: ["workers"],
  io_jobs: ["io-jobs"],
  ffmpeg_threads: ["ffmpeg-threads"],
  dry_run: ["dry-run"],
  no_cache: ["no-cache"],
};

const isCheckbox = el => el.type === "checkbox";

/**
 * `dispatch` sends an Intent and rejects with the core's refusal (app.js's `sendIntent`), and
 * `published(mode)` is the mode's draft in the View State applied last. Two guards keep a published draft from
 * undoing what the user is doing: a field with focus is never written, so typing keeps its caret,
 * and neither is a key whose edit is still on its way to the core, since a state published before
 * the core applied it holds the value from before the edit. The core answers a draft Intent once it
 * applied it, before it publishes it, so every state it publishes after the answer holds the edit.
 * A publication sent before the answer can still arrive after it, since Tauri does not order events
 * against invoke replies: an unfocused field then shows the old value until the publication the
 * edit armed, at most one interval (66 ms) later. That flicker is accepted.
 */
export function createSettingsDraft({ doc, dispatch, published }) {
  // Edits sent and not answered yet, by `${mode}:${key}`.
  const pending = new Map();

  /**
   * Sends the value `el` now holds as the draft's `key`; call it after setting a field from code.
   * A refused edit (a busy batch locks the draft) changed nothing in the core, which then
   * publishes nothing, so the field gets the published value back unless a later edit is pending.
   */
  async function send(mode, key, el) {
    const id = `${mode}:${key}`;
    pending.set(id, (pending.get(id) ?? 0) + 1);
    let refused = false;
    try {
      await dispatch({ type: "edit_draft", mode, changes: { [key]: isCheckbox(el) ? el.checked : el.value } });
    } catch (err) {
      console.error("[edit_draft] refused:", err);
      refused = true;
    } finally {
      if (pending.get(id) === 1) pending.delete(id);
      else pending.set(id, pending.get(id) - 1);
    }
    if (refused && !pending.has(id)) {
      const value = published(mode)?.[key];
      if (value !== undefined) apply(mode, { [key]: value }, { force: true });
    }
  }

  /**
   * Sends an edit on each change of every field the page has, and on each input of any field but a
   * checkbox, whose click already fires change.
   */
  function bind() {
    for (const mode of ["remux", "extract"]) {
      for (const [key, suffixes] of Object.entries(DRAFT_FIELDS)) {
        for (const suffix of suffixes) {
          const el = doc.getElementById(`${mode}-${suffix}`);
          if (!el) continue;
          const onEdit = () => { send(mode, key, el); };
          if (!isCheckbox(el)) el.addEventListener("input", onEdit);
          el.addEventListener("change", onEdit);
        }
      }
    }
  }

  /**
   * Writes `draft` into `mode`'s fields, but the guarded ones (any, with `force`). Returns the keys
   * of the fields it changed.
   */
  function apply(mode, draft, { force = false } = {}) {
    const written = new Set();
    for (const [key, value] of Object.entries(draft ?? {})) {
      if (!force && pending.has(`${mode}:${key}`)) continue;
      for (const suffix of DRAFT_FIELDS[key] ?? []) {
        const el = doc.getElementById(`${mode}-${suffix}`);
        if (!el || (!force && el === doc.activeElement)) continue;
        const property = isCheckbox(el) ? "checked" : "value";
        // A range resets a value that is not a number to its midpoint, so the complexity slider
        // takes only a parsed value and stays put while the box holds something else.
        const next = el.type === "range" ? String(parseInt(value, 10)) : value;
        if (next === "NaN" || el[property] === next) continue;
        el[property] = next;
        written.add(key);
      }
    }
    return written;
  }

  return { bind, apply, send };
}
