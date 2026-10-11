/**
 * The sidebar's Preset Profile rows: applies a profile's bitrate and complexity to the form, or
 * marks the form as hand-edited ("custom"). The engine owns the Preset Profile catalog: `load()`
 * fills this module's copy from it and renders one row per profile into `#preset-list`, so the
 * page holds no hardcoded copy of it.
 */
export function createPresetMenu({ invoke, doc }) {
  // Keyed by profile id; empty until `load()` has an answer.
  const catalog = {};
  // The form starts on the engine's defaults, which are the balanced profile's.
  let activeKey = "balanced";

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
      // The custom row shows the form's own values, which app.js keeps current.
      if (p.id === "custom") sub.id = "preset-custom-sub";
      sub.textContent = presetSubtitle(p.bitrate, p.complexity);
      text.append(p.name, sub);
      row.append(dot, text);
      list.appendChild(row);
    }
    setActiveProfileOption(activeKey);
  }

  async function load() {
    try {
      const profiles = (await invoke("get_preset_profiles")) ?? [];
      for (const p of profiles) {
        catalog[p.id] = { bitrate: p.bitrate, complexity: String(p.complexity) };
      }
      renderRows(profiles);
    } catch (err) {
      console.error("[get_preset_profiles] failed:", err);
    }
  }

  function setActiveProfileOption(presetKey) {
    activeKey = presetKey;
    doc.querySelectorAll(".profile-option-item").forEach(item => {
      const active = item.dataset.preset === presetKey;
      item.classList.toggle("active", active);
      item.setAttribute("aria-pressed", active ? "true" : "false");
    });
  }

  function apply(presetKey) {
    if (presetKey === "custom") {
      setActiveProfileOption("custom");
      return;
    }
    const p = catalog[presetKey];
    // Without its catalog entry there is nothing to apply.
    if (!p) return;
    setActiveProfileOption(presetKey);

    const setVal = (id, val) => {
      const el = doc.getElementById(id);
      if (el) el.value = val;
    };
    setVal("remux-bitrate", p.bitrate);
    setVal("remux-complexity", p.complexity);
    setVal("remux-cplx-val", p.complexity);
  }

  return { load, apply };
}

/** A preset row's subtitle: the bitrate and complexity a profile, or the custom form, encodes with. */
export function presetSubtitle(bitrate, complexity) {
  return `Opus ${bitrate} · complexity ${complexity}`;
}
