/**
 * The webview transport of the View State (ADR-0016, S4 amendment): what the page does with one
 * `view-state` payload, which is a whole View State or a patch of the one emitted before it
 * (`src-tauri/src/view_patch.rs` builds the patch; change the protocol there first). Transport
 * logic only, no DOM: the page renders the state this returns.
 *
 * A patch is `{seq, base, ...changed top-level keys}`, each a whole replacement value (`null`
 * included). `remux` and `extract` carry only their changed ModeView keys, with their rows as
 * either `rows` (the whole list) or `row_changes`, `[[index, Row], ...]`.
 */

const MODES = ["remux", "extract"];

/** Every row of both modes changed: a whole state, or rows sent whole. */
export const ALL_ROWS = Object.freeze({ remux: "all", extract: "all" });

/**
 * What to do with `payload` given `applied`, the View State applied last (null before the first):
 * - `{action: "drop"}`: not newer than `applied`;
 * - `{action: "full", state, rows: ALL_ROWS}`: a whole state, applied as it is;
 * - `{action: "merge", state, rows}`: a patch of `applied`. `state` is the merged View State,
 *   which shares the unchanged parts of `applied` and mutates none of them, and `rows[mode]` is
 *   `"all"` when the mode's rows were replaced, else the ascending indices of the rows that changed;
 * - `{action: "resync"}`: a patch of a state the page does not hold, so it must read `get_state`.
 */
export function nextState(applied, payload) {
  if (applied && payload.seq <= applied.seq) return { action: "drop" };
  if (!("base" in payload)) return { action: "full", state: payload, rows: ALL_ROWS };
  if (!applied || payload.base !== applied.seq) return { action: "resync" };
  const changes = { ...payload };
  delete changes.base;
  const state = { ...applied, ...changes };
  const rows = {};
  for (const mode of MODES) {
    rows[mode] = [];
    if (!changes[mode]) continue;
    const { row_changes: rowChanges, ...fields } = changes[mode];
    const view = { ...applied[mode], ...fields };
    if ("rows" in fields) {
      rows[mode] = "all";
    } else if (rowChanges) {
      // A copy, so the rows array the caller still holds keeps its rows.
      view.rows = [...view.rows];
      for (const [index, row] of rowChanges) view.rows[index] = row;
      rows[mode] = rowChanges.map(([index]) => index);
    }
    state[mode] = view;
  }
  return { action: "merge", state, rows };
}
