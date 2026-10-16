/**
 * The sidebar's Preset Profile rows, rendered from the View State: one row per profile of the
 * published catalog (`presets`), the remux Settings Draft's preset marked active, and the custom
 * row's subtitle showing the draft's own bitrate and complexity. A click sends `set_preset`; the
 * core applies the profile to the draft, so the page holds no copy of the catalog or its rules.
 */
export function createPresetMenu({ doc, dispatch, isLocked }) {
  // The catalog the rows were rendered from, as JSON.
  let rowsKey = null;

  function renderRows(profiles) {
    const list = doc.getElementById("preset-list");
    if (!list) return;
    list.querySelectorAll(".preset-row").forEach(row => row.remove());
    for (const p of profiles) {
      const row = doc.createElement("button");
      row.type = "button";
      row.className = "nav preset-row profile-option-item lock-while-job";
      row.dataset.preset = p.id;
      const dot = doc.createElement("span");
      dot.className = "preset-dot";
      dot.setAttribute("aria-hidden", "true");
      const text = doc.createElement("span");
      text.className = "nav-text";
      const sub = doc.createElement("small");
      // The custom row shows the draft's own values, which `render` keeps current.
      if (p.id === "custom") sub.id = "preset-custom-sub";
      sub.textContent = presetSubtitle(p.bitrate, p.complexity);
      text.append(p.name, sub);
      row.append(dot, text);
      list.appendChild(row);
    }
  }

  /** Renders `profiles` and the remux `draft` over them. */
  function render(profiles = [], draft = null) {
    const key = JSON.stringify(profiles);
    if (key !== rowsKey) {
      rowsKey = key;
      renderRows(profiles);
    }
    doc.querySelectorAll(".profile-option-item").forEach(item => {
      const active = item.dataset.preset === draft?.preset;
      item.classList.toggle("active", active);
      item.setAttribute("aria-pressed", active ? "true" : "false");
    });
    const sub = doc.getElementById("preset-custom-sub");
    // A complexity that is not a number yet (a cleared box) keeps the subtitle as it was.
    const complexity = parseInt(draft?.complexity, 10);
    if (sub && draft?.bitrate != null && !Number.isNaN(complexity)) {
      const text = presetSubtitle(draft.bitrate, complexity);
      if (sub.textContent !== text) sub.textContent = text;
    }
  }

  /** Sends `set_preset` for a clicked row, unless a job holds the form. */
  function bind() {
    doc.getElementById("preset-list")?.addEventListener("click", e => {
      const item = e.target.closest?.(".profile-option-item[data-preset]");
      if (!item || isLocked()) return undefined;
      // Presets set the Opus encode, and only Remux transcodes (#preset-list is remux-only).
      return dispatch({ type: "set_preset", mode: "remux", preset: item.dataset.preset });
    });
  }

  return { bind, render };
}

/** A preset row's subtitle: the bitrate and complexity a profile, or the custom form, encodes with. */
function presetSubtitle(bitrate, complexity) {
  return `Opus ${bitrate} · complexity ${complexity}`;
}
